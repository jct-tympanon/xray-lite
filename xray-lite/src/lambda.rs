use crate::{Error, Header, Result};

/// Read the global XRay header for the currently executing lambda invocation.
/// ## Errors
/// - [`Error::MissingEnvVar`] if the lambda environment doesn't contain XRay information in _X_AMZN_TRACE_ID
/// - [`Error::BadConfig`] if _X_AMZN_TRACE_ID is defined but can't be parsed
pub fn header() -> Result<Header> {
    std::env::var("_X_AMZN_TRACE_ID")
        .map_err(|_| Error::MissingEnvVar("_X_AMZN_TRACE_ID"))?
        .parse::<Header>()
        .map_err(|e| Error::BadConfig(format!("invalid X-Ray trace ID header value: {e}")))
}
