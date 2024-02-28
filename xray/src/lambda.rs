use crate::{Error, Header, Result};
use std::{
    env::var,
    fs::{create_dir_all, File},
};

pub(crate) fn init() -> std::io::Result<()> {
    if taskRoot().is_some() {
        create_dir_all("/tmp/.aws-xray")?;
        File::create("/tmp/.aws-xray/initialized")?;
    }
    Ok(())
}

pub(crate) fn taskRoot() -> Option<String> {
    var("LAMBDA_TASK_ROOT").ok()
}

pub(crate) fn header() -> Result<Header> {
    var("_X_AMZN_TRACE_ID")
        .map_err(|_| Error::MissingEnvVar(&"_X_AMZN_TRACE_ID"))?
        .parse::<Header>()
        .map_err(|e| Error::BadConfig(format!("invalid X-Ray trace ID header value: {e}")))
}
