//! provides the [`ClassifyAwsIntercept`] SDK interceptor, which propagates trace context to downstream
//! SDK calls and publishes SDK operation segments to the lambda XRay daemon. AWS service operations
//! are recognized by an instance of [`RequestClassifier`].
//! 
//! The behavior of this interceptor is designed to be best-effort. Failure to collect or transmit XRay segments
//! or trace data will not panic or disrupt request processing.
//! 
//! ## Example
//! ```no_run
//! use aws_config::BehaviorVersion;
//! use xray_lite_aws_sdk::ClassifyAwsIntercept;
//!
//! async fn get_object_from_s3() {
//!     let classifier = ClassifyAwsIntercept::from_lambda_env().unwrap();
//!
//!     let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
//!     let s3_config = aws_sdk_s3::Config::new(&sdk_config).to_builder().interceptor(classifier).build();
//!     let s3_client = aws_sdk_s3::Client::from_conf(s3_config);
//!     s3_client
//!         .get_object()
//!         .bucket("the-bucket-name")
//!         .key("the-object-key")
//!         .send()
//!         .await
//!         .unwrap();
//! }
//! ```

use std::fmt::Debug;
use std::sync::RwLock;

use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::{
    interceptors::{
        Intercept,
        context::{
            BeforeTransmitInterceptorContextMut, 
            FinalizerInterceptorContextRef,
        }
    },
    runtime_components::RuntimeComponents,
};
use aws_smithy_runtime_api::http::Request;
use aws_smithy_types::config_bag::{ConfigBag, Storable, StoreReplace};
use aws_types::request_id::RequestId;
use url::Url;
use xray_lite::{AwsNamespace, Client as XRayClient, Context, DaemonClient, Error as XRayError, Header, SubsegmentContext, SubsegmentSession};

/// helper to extract the first value from a key-value pair iterator where the key
/// matches the given name, case-insensitive. Works for both Url query parameters and
/// HTTP headers.
macro_rules! first_with_name {
    ($pairs:expr, $name:expr) => {
        $pairs.filter_map(
            |(name, value)| 
            if name.to_lowercase() == $name {
                Some(value)
            } else {
                None
            }
         )
         .next()
    };
}

/// A strategy to decode an outbound SDK request into an instance of [`AwsNamespace`].
pub trait RequestClassifier: Debug + Send + Sync {
    /// If possible, identify the AWS service and operation targeted by the request.
    /// ## Returns
    /// - Some if the request could be classified by this strategy
    /// - None otherwise
    fn classify_request(&self, request: &Request) -> Option<AwsNamespace>;
}

/// A [`RequestClassifier`] which works for a number of known AWS services.
/// 
/// If the outbound request includes an "x-amz-target" header, that header value is used. This covers
/// DynamoDB, Cognito, SQS, and some others. There is no official standard for this header, so the
/// exact set of suppported services is difficult to determine.
/// 
/// Otherwise if the outbound request targets an S3 endpoint, the x-id parameter is used (if present).
/// 
/// Otherwise, None is returned.
#[derive(Debug)]
pub struct KnownServices;
impl RequestClassifier for KnownServices {
    fn classify_request(&self, request: &Request) -> Option<AwsNamespace> {
        // many services use x-amz-target, but this is not universal. if it is present, use it directly.
        if let Some(target) = first_with_name!(request.headers().iter(), "x-amz-target") {
            let parts: Vec<&str> = target.split('.').collect();
            return match parts.len() {
                // [service].[operation]
                2 => Some(AwsNamespace::new(parts[0], parts[1])),
                // unknown usage.
                _ => None
            };
        }

        // otherwise, implement well-known schemes based on the service endpoint.
        let aws_url = Url::try_parse_aws_url(request.uri())?;
        let service_code = aws_url.aws_service_code()?;
        match service_code {
            "s3" => S3RequestClassifier::classify_url(&aws_url),
            _ => None,
        }
    }
}

/// An S3-specific namespace classifier that works with aws-sdk-s3 1.x.
#[derive(Debug)]
pub struct S3RequestClassifier;
impl S3RequestClassifier {
    /// Classify the given URL if it targets an S3 endpoint and specifies the 'x-id' parameter.
    /// Otherwise, returns None.
    pub fn classify_url(url: &Url) -> Option<AwsNamespace> {
        url.aws_service_code()
            .filter(|code| *code == "s3")
            .and_then(|_| first_with_name!(url.query_pairs(), "x-id"))
            .map(|op_name| AwsNamespace::new("S3", op_name))
    }
}
impl RequestClassifier for S3RequestClassifier {
    fn classify_request(&self, request: &Request) -> Option<AwsNamespace> {
        Url::try_parse_aws_url(request.uri())
            .and_then(|u| Self::classify_url(&u))
    }
}

/// A helper trait for parsing AWS endpoint URLs.
trait AwsServiceUrl {
    fn try_parse_aws_url(url: &str) -> Option<Self>
        where Self: Sized;

    /// Extract the service-specific part of this Url, as documented in https://docs.aws.amazon.com/general/latest/gr/rande.html
    fn aws_service_code(&self) -> Option<&str>;
}
impl AwsServiceUrl for Url {
    fn try_parse_aws_url(url: &str) -> Option<Self> {
        match url.parse::<Url>() {
            Ok(url) if url.domain().is_some_and(|endpoint| endpoint.ends_with(".amazonaws.com")) => Some(url),
            _ => None
        }
    }

    fn aws_service_code(&self) -> Option<&str> {
        let components: Vec<&str> = self.domain()?.split('.').collect();
        match components.len() {
            5 => Some(components[1]), // resource-specific endpoints, e.g. https://{bucket-name}.s3.{region}.amazonaws.com
            3 | 4 => Some(components[0]), // regional or global endpoints
            _ => None
        }
    }
}

/// Adapts an instance of [`SubsegmentSession`] so that it can be stored in
/// [`ConfigBag::interceptor_state`].
#[derive(Debug)]
struct CurrentSubsegment<C>(RwLock<SubsegmentSession<C, AwsNamespace>>)
    where C: XRayClient + 'static;

impl<C> Storable for CurrentSubsegment<C> 
    where C: XRayClient + 'static
{
    type Storer = StoreReplace<Self>;
}
impl<C> CurrentSubsegment<C>
    where C: XRayClient + 'static
{
    /// Record the response status and request ID on the current SubsegmentSession.
    fn finalize(&self, context: &FinalizerInterceptorContextRef<'_>) 
    {
        if let Ok(mut session) = self.0.write() {
            if let Some(namespace) = session.namespace_mut() {
                if let Some(response) = context.response() {
                    namespace.response_status(response.status().as_u16());
                    if let Some(request_id) = response.request_id() {
                        namespace.request_id(request_id);
                    }
                }
            }
        }
    }
}

/// A Smithy interceptor which publishes trace segments to the lambda XRay daemon for any recognized AWS operations.
#[derive(Debug)]
pub struct ClassifyAwsIntercept<C: XRayClient + 'static, I: RequestClassifier> {
    client: C,
    classifier: I,
}
impl<C, I> ClassifyAwsIntercept<C, I> 
    where C: XRayClient + 'static,
          I: RequestClassifier + 'static
{
    /// Create the interceptor using a [`DaemonClient`] and an instance of [`KnownServices`] to classify outbound AWS requests.
    /// ## Returns
    /// - Err if the client could not be initialized
    pub fn from_lambda_env() -> Result<ClassifyAwsIntercept<DaemonClient, KnownServices>, XRayError> {
        let client = DaemonClient::from_lambda_env()?;
        Ok(ClassifyAwsIntercept::new(client, KnownServices))
    }

    /// Create the interceptor using a provided [`XRayClient`] and [`RequestClassifier`]. The [`RequestClassifier`] is responsible
    /// for identifying the AWS Service and Operation of each outbound SDK operation.
    pub fn new(client: C, classifier: I) -> Self {
        Self { client, classifier }
    }
}

impl<C: XRayClient, I: RequestClassifier> Intercept for ClassifyAwsIntercept<C, I>
{
    fn name(&self) -> &'static str {
        "XRayIntercept"
    }

    fn modify_before_transmit(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        if let Some(op) = self.classifier.classify_request(context.request()) {
            if let Ok(segment) = SubsegmentContext::from_lambda_env(self.client.clone()) {
                let session = segment.enter_subsegment(op);
                if let Some(trace_id) = session.x_amzn_trace_id() {
                    context
                        .request_mut()
                        .headers_mut()
                        .insert(Header::NAME, trace_id);
                }
                cfg.interceptor_state().store_put(CurrentSubsegment(RwLock::new(session)));
            }
        }
        Ok(())
    }

    fn read_after_attempt(
        &self,
        context: &FinalizerInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {

        if let Some(session) = cfg.interceptor_state().load::<CurrentSubsegment<C>>() {
            session.finalize(context);
            // remove segment from the bag so that it can be dropped and transmitted to the daemon.
            cfg.interceptor_state().unset::<CurrentSubsegment<C>>();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use std::{env, sync::{Arc, Mutex}};

    use aws_config::BehaviorVersion;
    use aws_sdk_dynamodb::types::AttributeValue;
    use aws_sdk_s3::config::http::HttpRequest;
    use aws_smithy_runtime::client::http::test_util::{ReplayEvent, StaticReplayClient};
    use aws_smithy_types::body::SdkBody;
    use serde::Serialize;
    use serde_json::json;
    use serial_test::serial;
    use url::Url;
    use xray_lite::{AwsNamespace, Client};

    use super::{AwsServiceUrl, ClassifyAwsIntercept, KnownServices, S3RequestClassifier};

    macro_rules! test_sdk_client {
        ($client_crate:ident, $replay_client:expr, $xray_client:expr) => {
            $client_crate::Client::from_conf(
                $client_crate::Config::builder()
                    .behavior_version(BehaviorVersion::latest())
                    .credentials_provider($client_crate::config::Credentials::new(
                        "ATESTCLIENT",
                        "astestsecretkey",
                        Some("atestsessiontoken".to_string()),
                        None,
                        "test-credentials",
                    ))
                    .region($client_crate::config::Region::new("us-east-1"))
                    .http_client($replay_client.clone())
                    .interceptor(ClassifyAwsIntercept::new($xray_client.clone(), KnownServices))
                    .build(),
            )
        };
    }

    /// a mock XRay daemon client which accumulates messages in memory, for post-test verification.
    #[derive(Default, Debug, Clone)]
    struct TestXRayClient {
        messages: Arc<Mutex<Vec<serde_json::Value>>>
    }
    impl Client for TestXRayClient {
        fn send<S>(&self, data: &S) -> xray_lite::Result<()>
            where S: Serialize 
        {
            let json = serde_json::to_value(data)?;
            self.messages.lock().unwrap().push(json);
            Ok(())
        }
    }

    #[test]
    fn parse_global_url() {
        let url = Url::try_parse_aws_url("https://s3.amazonaws.com").unwrap();
        assert_eq!("https://s3.amazonaws.com/", url.as_str());
        assert_eq!(Some("s3"), url.aws_service_code());
    }

    #[test]
    fn parse_regional_url() {
        let url = Url::try_parse_aws_url("https://s3.us-west-2.amazonaws.com").unwrap();
        assert_eq!("https://s3.us-west-2.amazonaws.com/", url.as_str());
        assert_eq!(Some("s3"), url.aws_service_code());
    }

    #[test]
    fn parse_non_aws_url() {
        assert_eq!(None, Url::try_parse_aws_url("https://s3.us-west-2.amazon.com"));
    }

    #[test]
    fn classify_s3_url() {
        let url = Url::try_parse_aws_url("https://s3.us-west-2.amazonaws.com/test-bucket/test-key?x-id=GetObject").unwrap();
        assert_eq!(
            format!("{:?}", AwsNamespace::new("S3", "GetObject")), 
            format!("{:?}", S3RequestClassifier::classify_url(&url).unwrap()), 
        );
    }

    #[test]
    fn classify_s3_bucket_url() {
        let url = Url::try_parse_aws_url("https://test-bucket.s3.us-west-2.amazonaws.com/test-key?x-id=GetObject").unwrap();
        assert_eq!(
            format!("{:?}", AwsNamespace::new("S3", "GetObject")), 
            format!("{:?}", S3RequestClassifier::classify_url(&url).unwrap()), 
        );
    }

    #[test]
    fn classify_unknown_s3_url() {
        let url = Url::try_parse_aws_url("https://s3.us-west-2.amazonaws.com/test-bucket/test-key").unwrap();
        assert!(S3RequestClassifier::classify_url(&url).is_none());
    }

    // tests dependent on static std::env setup must be run serially.

    #[tokio::test] #[serial]
    async fn no_trace_id() {
        let replay = StaticReplayClient::new(vec![s3_get_object("test-bucket", "some/key", None)]);
        let xray_client = TestXRayClient::default();
        let s3_client = test_sdk_client!(aws_sdk_s3, replay, xray_client);

        env::remove_var("_X_AMZN_TRACE_ID");
        s3_client.get_object()
            .bucket("test-bucket").key("some/key")
            .send().await.unwrap();

        // no trace data found in the environment.
        assert_eq!(0, xray_client.messages.lock().unwrap().len());

        replay.relaxed_requests_match();
    }

    #[tokio::test] #[serial]
    async fn classify_s3() {
        let replay = StaticReplayClient::new(vec![
            s3_get_object("test-bucket", "some/key", Some("Root=1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb"))
        ]);
        let xray_client = TestXRayClient::default();
        let s3_client = test_sdk_client!(aws_sdk_s3, replay, xray_client);

        env::set_var("_X_AMZN_TRACE_ID", "Root=1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb");
        s3_client.get_object()
            .bucket("test-bucket").key("some/key")
            .send().await.unwrap();

        let mut received_messages = xray_client.messages.lock().unwrap().clone();
        let segment_id = received_messages[0].get("id").unwrap().as_str().unwrap().to_string();
        // replace variable outputs with static values prior to assertions.
        normalize_messages(&mut received_messages);

        assert_eq!(
            vec![
                json!({
                    "name": "S3", "id": segment_id, 
                    "start_time": 0.0, "trace_id": "1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb", 
                    "in_progress": true,
                    "namespace": "aws", "type": "subsegment", "aws": {"operation": "GetObject"},
                }),
                json!({
                    "name": "S3", "id": segment_id, 
                    "start_time": 0.0, "end_time": 1.0, "trace_id": "1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb", 
                    "namespace": "aws", "type": "subsegment", "http": {"response": {"status": 200}}, "aws": {"operation": "GetObject"}
                })],
            received_messages
        );

        let requests: Vec<&HttpRequest> = replay.actual_requests().collect();
        assert_eq!(1, requests.len());
        assert_eq!(
            requests[0].headers().get("X-Amzn-Trace-Id").unwrap(), 
            format!("Root=1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb;Parent={segment_id}")
        );
        replay.assert_requests_match(&["x-amz-user-agent", "authorization", "x-amzn-trace-id"]);
    }

    #[tokio::test] #[serial]
    async fn classify_ddb() {
        let replay = StaticReplayClient::new(vec![
            ReplayEvent::new(
                http::Request::builder()
                    .method("POST")
                    .uri("https://dynamodb.us-east-1.amazonaws.com/")
                    .body(SdkBody::empty())
                    .unwrap(),
                http::Response::builder()
                    .status(404).body("Not Found".into())
                    .unwrap(),
            )
        ]);
        let xray_client = TestXRayClient::default();
        let ddb_client = test_sdk_client!(aws_sdk_dynamodb, replay, xray_client);

        env::set_var("_X_AMZN_TRACE_ID", "Root=1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb");
        let _ = ddb_client.get_item()
            .table_name("Foo").key("bar", AttributeValue::S("baz".into()))
            .send().await;

        let mut received_messages = xray_client.messages.lock().unwrap().clone();
        let segment_id = received_messages[0].get("id").unwrap().as_str().unwrap().to_string();
        // replace variable outputs with static values prior to assertions.
        normalize_messages(&mut received_messages);

        assert_eq!(
            vec![
                json!({
                    "name": "DynamoDB_20120810", "id": segment_id, 
                    "start_time": 0.0, "trace_id": "1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb", 
                    "in_progress": true,
                    "namespace": "aws", "type": "subsegment", "aws": {"operation": "GetItem"},
                }),
                json!({
                    "name": "DynamoDB_20120810", "id": segment_id, 
                    "start_time": 0.0, "end_time": 1.0, "trace_id": "1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb", 
                    "namespace": "aws", "type": "subsegment", "http": {"response": {"status": 404}}, "aws": {"operation": "GetItem"}
                })],
            received_messages
        );
        let requests: Vec<&HttpRequest> = replay.actual_requests().collect();
        assert_eq!(1, requests.len());
        assert_eq!(
            requests[0].headers().get("X-Amzn-Trace-Id").unwrap(), 
            format!("Root=1-aaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbb;Parent={segment_id}")
        );
    }

    fn s3_get_object(bucket: &str, key: &str, trace_id: Option<&str>) -> ReplayEvent {
        let mut request = http::Request::builder()
            .method("GET")
            .uri(format!("https://{bucket}.s3.us-east-1.amazonaws.com/{key}?x-id=GetObject"))
            .body(SdkBody::empty())
            .unwrap();
        if let Some(id) = trace_id {
            request.headers_mut().insert("X-Amzn-Trace-Id", id.parse().unwrap());
        }
        ReplayEvent::new(request,
            http::Response::builder()
                .status(200).body(SdkBody::from("hello, world"))
                .unwrap(),
        )
    }

    /// change time-sensitive outputs to static values, for test verification.
    fn normalize_messages(messages: &mut Vec<serde_json::Value>) {
        for message in messages {
            *message.get_mut("start_time").unwrap() = json!(0.0);
            if let Some(t) = message.get_mut("end_time") {
                *t = json!(1.0);
            }
        }
    }

}