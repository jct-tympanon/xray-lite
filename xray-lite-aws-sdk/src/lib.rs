#![warn(missing_docs)]

//! Extension of `xray-lite` for [AWS SDK for Rust](https://aws.amazon.com/sdk-for-rust/).
//!
//! With this crate, you can easily add the X-Ray tracing capability to your
//! AWS service requests through
//! [AWS SDK for Rust](https://aws.amazon.com/sdk-for-rust/).
//! It utilizes the [interceptor](https://docs.rs/aws-smithy-runtime-api/latest/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html)
//! which can be attached to `CustomizableOperation` available via the
//! `customize` method of any request builder; e.g.,
//! [`aws_sdk_s3::operation::get_object::builders::GetObjectFluentBuilder::customize`](https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/operation/get_object/builders/struct.GetObjectFluentBuilder.html#method.customize)
//!
//! The following example shows how to report a subsegment for each attempt of
//! the S3 GetObject operation:
//! ```no_run
//! use aws_config::BehaviorVersion;
//! use xray_lite::{DaemonClient, SubsegmentContext};
//! use xray_lite_aws_sdk::ContextExt as _;
//!
//! async fn get_object_from_s3() {
//!     let xray_client = DaemonClient::from_lambda_env().unwrap();
//!     let xray_context = SubsegmentContext::from_lambda_env(xray_client).unwrap();
//!
//!     let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
//!     let s3_client = aws_sdk_s3::Client::new(&config);
//!     s3_client
//!         .get_object()
//!         .bucket("the-bucket-name")
//!         .key("the-object-key")
//!         .customize()
//!         .interceptor(xray_context.intercept_operation("S3", "GetObject"))
//!         .send()
//!         .await
//!         .unwrap();
//! }
//! ```

use std::sync::{Arc, Mutex};

use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::interceptors::context::{
    BeforeTransmitInterceptorContextMut, BeforeTransmitInterceptorContextRef,
    FinalizerInterceptorContextRef,
};
use aws_smithy_runtime_api::client::interceptors::Intercept;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::config_bag::ConfigBag;
use aws_types::request_id::RequestId;

use xray_lite::{AwsNamespace, Context, Header, SubsegmentSession};

#[cfg(feature = "classify")]
pub mod classify;

/// Extension of [`Context`].
///
/// This trait is automatically implemented for any [`Context`] that satisfies
/// the bounds.
pub trait ContextExt: Context + Clone + std::fmt::Debug + Send + Sync + 'static {
    /// Creates an [`Intercept`](https://docs.rs/aws-smithy-runtime-api/1.1.7/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html)
    /// for the AWS service request.
    ///
    /// A returned `Intercept` implements the following hooks:
    /// 1. [`read_before_attempt`](https://docs.rs/aws-smithy-runtime-api/1.1.7/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html#method.read_before_attempt):
    ///    Starts a subsegment of the AWS service request
    /// 2. [`modify_before_transmit`](https://docs.rs/aws-smithy-runtime-api/1.1.7/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html#method.modify_before_transmit):
    ///    Injects the `X-Amzn-Trace-Id` header into the request
    /// 3. [`read_after_attempt`](https://docs.rs/aws-smithy-runtime-api/1.1.7/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html#method.read_after_attempt):
    ///    Updates the subsegment with the request ID and the response status,
    ///    and reports the subsegment to the X-Ray daemon
    fn intercept_operation(
        &self,
        service: impl Into<String>,
        operation: impl Into<String>,
    ) -> impl Intercept + 'static {
        XrayIntercept::new_with_operation(self.clone(), service, operation)
    }
}

impl<T> ContextExt for T where T: Context + Clone + std::fmt::Debug + Send + Sync + 'static {}

#[derive(Debug)]
struct XrayIntercept<T>
where
    T: Context + Clone + std::fmt::Debug + Send + Sync + 'static,
{
    context: T,
    service: String,
    operation: String,
    // session is unnecessarily wrapped in Mutex because `Intercept` is
    // immutable during its method calls.
    #[allow(clippy::type_complexity)]
    session: Arc<Mutex<Option<SubsegmentSession<T::Client, AwsNamespace>>>>,
}

impl<T> XrayIntercept<T>
where
    T: Context + Clone + std::fmt::Debug + Send + Sync + 'static,
{
    fn new_with_operation(
        context: T,
        service: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self {
            context,
            service: service.into(),
            operation: operation.into(),
            session: Arc::new(Mutex::new(None)),
        }
    }
}

impl<T> Intercept for XrayIntercept<T>
where
    T: Context + Clone + std::fmt::Debug + Send + Sync + 'static,
{
    fn name(&self) -> &'static str {
        "XrayIntercept"
    }

    fn read_before_attempt(
        &self,
        _context: &BeforeTransmitInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let session = self.context.enter_subsegment(AwsNamespace::new(
            self.service.clone(),
            self.operation.clone(),
        ));
        *self.session.lock().unwrap() = Some(session);
        Ok(())
    }

    fn modify_before_transmit(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let trace_id = self
            .session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.x_amzn_trace_id());
        if let Some(trace_id) = trace_id {
            context
                .request_mut()
                .headers_mut()
                .insert(Header::NAME, trace_id);
        }
        Ok(())
    }

    fn read_after_attempt(
        &self,
        context: &FinalizerInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let mut session = self.session.lock().unwrap();
        if let Some(mut session) = session.take() {
            if let Some(namespace) = session.namespace_mut() {
                if let Some(response) = context.response() {
                    namespace.response_status(response.status().as_u16());
                    if let Some(request_id) = response.request_id() {
                        namespace.request_id(request_id);
                    }
                }
            }
        }
        Ok(())
    }
}
