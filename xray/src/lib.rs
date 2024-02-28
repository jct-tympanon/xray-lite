#![warn(missing_docs)]
//#![deny(warnings)]
//! Provides a client interface for [AWS X-Ray](https://aws.amazon.com/xray/)

use serde::Serialize;
use std::{
    env,
    net::{SocketAddr, UdpSocket},
    result::Result as StdResult,
    sync::Arc,
};

mod epoch;
mod error;
mod header;
mod hexbytes;
mod lambda;
mod segment;
mod segment_id;
mod trace_id;

pub use crate::{
    epoch::Seconds, error::Error, header::Header, segment::*, segment_id::SegmentId,
    trace_id::TraceId,
};

/// Type alias for Results which may return `xray::Errors`
pub type Result<T> = StdResult<T, Error>;

/// X-Ray daemon client interface
#[derive(Clone, Debug)]
pub struct Client {
    socket: Arc<UdpSocket>,
}

impl Client {
    const HEADER: &'static [u8] = br#"{"format": "json", "version": 1}"#;
    const DELIMITER: &'static [u8] = &[b'\n'];

    /// Return a new X-Ray client connected
    /// to the provided `addr`
    pub fn new(addr: SocketAddr) -> Result<Self> {
        let socket = Arc::new(UdpSocket::bind(&[([0, 0, 0, 0], 0).into()][..])?);
        socket.set_nonblocking(true)?;
        socket.connect(&addr)?;
        Ok(Client { socket })
    }

    /// Creates a new X-Ray client from the Lambda environment variable.
    ///
    /// The following environment variable must be set:
    /// - `AWS_XRAY_DAEMON_ADDRESS`: X-Ray daemon address
    ///
    /// Please refer to the [AWS documentation](https://docs.aws.amazon.com/lambda/latest/dg/configuration-envvars.html#configuration-envvars-runtime)
    /// for more details.
    pub fn from_lambda_env() -> Result<Self> {
        let addr: SocketAddr = env::var("AWS_XRAY_DAEMON_ADDRESS")
            .map_err(|_| Error::MissingEnvVar(&"AWS_XRAY_DAEMON_ADDRESS"))?
            .parse::<SocketAddr>()
            .map_err(|e| Error::BadConfig(format!("invalid X-Ray daemon address: {e}")))?;
        Client::new(addr)
    }

    #[inline]
    fn packet<S>(data: S) -> Result<Vec<u8>>
    where
        S: Serialize,
    {
        let bytes = serde_json::to_vec(&data)?;
        Ok([Self::HEADER, Self::DELIMITER, &bytes].concat())
    }

    /// send a segment to the xray daemon this client is connected to
    pub fn send<S>(
        &self,
        data: &S,
    ) -> Result<()>
    where
        S: Serialize,
    {
        self.socket.send(&Self::packet(data)?)?;
        Ok(())
    }
}

/// Context.
pub trait Context {
    /// Enters in a new subsegment.
    ///
    /// [`SubsegmentSession`] records the end of the subsegment when it is
    /// dropped.
    fn enter_subsegment(&self, name: String) -> SubsegmentSession;
}

/// Context as a subsegment of an existing segment.
#[derive(Debug)]
pub struct SubsegmentContext {
    client: Client,
    header: Header,
    name_prefix: String,
}

impl SubsegmentContext {
    /// Creates a new context from the Lambda environment variable.
    ///
    /// The following environment variable must be set:
    /// - `_X_AMZN_TRACE_ID`: AWS X-Ray trace ID
    ///
    /// Please refer to the [AWS documentation](https://docs.aws.amazon.com/lambda/latest/dg/configuration-envvars.html#configuration-envvars-runtime)
    /// for more details.
    pub fn from_lambda_env(client: Client) -> Result<Self> {
        let header = lambda::header()?;
        Ok(Self {
            client,
            header,
            name_prefix: "".to_string(),
        })
    }

    /// Updates the context with a given name prefix.
    pub fn with_name_prefix(self, prefix: impl Into<String>) -> Self {
        Self {
            client: self.client,
            header: self.header,
            name_prefix: prefix.into(),
        }
    }
}

impl Context for SubsegmentContext {
    fn enter_subsegment(&self, name: String) -> SubsegmentSession {
        SubsegmentSession::new(
            self.client.clone(),
            &self.header,
            format!("{}{}", self.name_prefix, name),
        )
    }
}

/// Subsegment session.
pub enum SubsegmentSession {
    /// Entered subsegment.
    Entered {
        client: Client,
        header: Header,
        subsegment: Subsegment,
    },
    /// Failed subsegment.
    Failed,
}

impl SubsegmentSession {
    fn new(client: Client, header: &Header, name: impl Into<String>) -> Self {
        let subsegment = Subsegment::begin(
            header.trace_id.clone(),
            header.parent_id.clone(),
            name,
        );
        match client.send(&subsegment) {
            Ok(_) => Self::Entered {
                client,
                header: header.with_parent_id(subsegment.id.clone()),
                subsegment,
            },
            Err(_) => Self::Failed,
        }
    }

    /// Returns the `x-amzn-trace-id` header value.
    pub fn x_amzn_trace_id(&self) -> Option<String> {
        match self {
            Self::Entered { header, .. } => Some(header.to_string()),
            Self::Failed => None,
        }
    }
}

impl Drop for SubsegmentSession {
    fn drop(&mut self) {
        match self {
            Self::Entered { client, subsegment, .. } => {
                subsegment.end();
                let _ = client.send(subsegment)
                    .map_err(|e| eprintln!("failed to end subsegment: {e}"));
            }
            Self::Failed => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_prefixes_packets_with_header() {
        assert_eq!(
            Client::packet(serde_json::json!({
                "foo": "bar"
            }))
            .unwrap(),
            [
                br#"{"format": "json", "version": 1}"# as &[u8],
                &[b'\n'],
                br#"{"foo":"bar"}"#,
            ].concat()
        )
    }
}
