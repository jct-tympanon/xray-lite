use serde_json::Error as JsonError;
use std::io::Error as IOError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO Error")]
    IO(#[from] IOError),
    #[error("Json Error")]
    Json(#[from] JsonError),
}
