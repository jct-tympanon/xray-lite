#![warn(missing_docs)]
//#![deny(warnings)]
//! Provides a client interface for [AWS X-Ray](https://aws.amazon.com/xray/)
//!
//! ### Examples
//!
//! #### Subsegment of AWS service operation
//!
//! **The [`xray_lite_aws_sdk`](https://docs.rs/xray-lite-aws-sdk) extension is
//! recommended for tracing operations through
//! [AWS SDK for Rust](https://aws.amazon.com/sdk-for-rust/).**
//!
//! Here is an example to record a subsegment of an AWS service operation
//! within a Lambda function invocation instrumented with AWS X-Ray:
//!
//! ```
//! use xray_lite::{AwsNamespace, Context, DaemonClient, SubsegmentContext};
//!
//! fn main() {
//!    // reads AWS_XRAY_DAEMON_ADDRESS
//!    # std::env::set_var("AWS_XRAY_DAEMON_ADDRESS", "127.0.0.1:2000");
//!    let client = DaemonClient::from_lambda_env().unwrap();
//!    // reads _X_AMZN_TRACE_ID
//!    # std::env::set_var("_X_AMZN_TRACE_ID", "Root=1-65dfb5a1-0123456789abcdef01234567;Parent=0123456789abcdef;Sampled=1");
//!    let context = SubsegmentContext::from_lambda_env(client).unwrap();
//!
//!    do_s3_get_object(&context);
//! }
//!
//! fn do_s3_get_object(context: &impl Context) {
//!     // subsegment will have the name "S3" and `aws.operation` "GetObject"
//!     let subsegment = context.enter_subsegment(AwsNamespace::new("S3", "GetObject"));
//!
//!     // call S3 GetObject ...
//!
//!     // if you are using `aws-sdk-s3` crate, you can update the subsegment
//!     // with the request ID. suppose `out` is the output of the `GetObject`
//!     // operation:
//!     //
//!     //     subsegment
//!     //         .namespace_mut()
//!     //         .zip(out.request_id())
//!     //         .map(|(ns, id)| ns.request_id(id));
//!
//!     // the subsegment will be ended and reported when it is dropped
//! }
//! ```
//!
//! #### Subsegment of remote service call
//!
//! Here is an example to record a subsegment of a remote service call within a
//! Lambda function invocation intstrumented with AWS X-Ray:
//!
//! ```
//! use xray_lite::{Context, DaemonClient, RemoteNamespace, SubsegmentContext};
//!
//! fn main() {
//!    // reads AWS_XRAY_DAEMON_ADDRESS
//!    # std::env::set_var("AWS_XRAY_DAEMON_ADDRESS", "127.0.0.1:2000");
//!    let client = DaemonClient::from_lambda_env().unwrap();
//!    // reads _X_AMZN_TRACE_ID
//!    # std::env::set_var("_X_AMZN_TRACE_ID", "Root=1-65dfb5a1-0123456789abcdef01234567;Parent=0123456789abcdef;Sampled=1");
//!    let context = SubsegmentContext::from_lambda_env(client).unwrap();
//!
//!    do_some_request(&context);
//! }
//!
//! fn do_some_request(context: &impl Context) {
//!     // subsegment will have the name "readme example",
//!     // `http.request.method` "POST", and `http.request.url` "https://codemonger.io/"
//!     let subsegment = context.enter_subsegment(RemoteNamespace::new(
//!         "readme example",
//!         "GET",
//!         "https://codemonger.io/",
//!     ));
//!
//!     // do some request ...
//!
//!     // the subsegment will be ended and reported when it is dropped
//! }
//! ```
//!
//! #### Custom subsegment
//!
//! Here is an example to record a custom subsegment within a Lambda function
//! invocation intstrumented with AWS X-Ray:
//!
//! ```
//! use xray_lite::{Context, DaemonClient, CustomNamespace, SubsegmentContext};
//!
//! fn main() {
//!    // reads AWS_XRAY_DAEMON_ADDRESS
//!    # std::env::set_var("AWS_XRAY_DAEMON_ADDRESS", "127.0.0.1:2000");
//!    let client = DaemonClient::from_lambda_env().unwrap();
//!    // reads _X_AMZN_TRACE_ID
//!    # std::env::set_var("_X_AMZN_TRACE_ID", "Root=1-65dfb5a1-0123456789abcdef01234567;Parent=0123456789abcdef;Sampled=1");
//!    let context = SubsegmentContext::from_lambda_env(client).unwrap()
//!        .with_name_prefix("readme_example.");
//!
//!    do_something(&context);
//! }
//!
//! fn do_something(context: &impl Context) {
//!     // subsegment will have the name "readme_example.do_something"
//!     let subsegment = context.enter_subsegment(CustomNamespace::new("do_something"));
//!
//!     // do some thing ...
//!
//!     // the subsegment will be ended and reported when it is dropped
//! }
//! ```
//!
//! ### Acknowledgements
//!
//! This crate is based on the [great work](https://github.com/softprops/xray)
//! by [Doug Tangren (softprops)](https://github.com/softprops).

mod client;
mod context;
mod epoch;
mod error;
mod header;
mod hexbytes;
mod lambda;
mod namespace;
mod segment;
mod segment_id;
mod session;
mod trace_id;

pub use crate::{
    client::{Client, DaemonClient, InfallibleClient, IntoInfallibleClient},
    context::{Context, InfallibleContext, IntoInfallibleContext, SubsegmentContext},
    epoch::Seconds,
    error::{Error, Result},
    header::{Header, SamplingDecision},
    lambda::header,
    namespace::{AwsNamespace, CustomNamespace, Namespace, RemoteNamespace},
    segment::*,
    segment_id::SegmentId,
    session::SubsegmentSession,
    trace_id::TraceId,
};
