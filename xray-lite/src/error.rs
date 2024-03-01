use serde_json::Error as JsonError;
use std::io::Error as IOError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("missing environment variable: {0}")]
    MissingEnvVar(&'static str),
    #[error("IO Error")]
    IO(#[from] IOError),
    #[error("Json Error")]
    Json(#[from] JsonError),
    #[error("bad configuration: {0}")]
    BadConfig(String),
}
