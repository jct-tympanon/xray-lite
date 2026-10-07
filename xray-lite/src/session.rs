//! Subsegment session management.

use crate::client::Client;
use crate::header::{Header, SamplingDecision};
use crate::namespace::Namespace;
use crate::segment::Subsegment;

/// Subsegment session.
#[derive(Debug)]
pub enum SubsegmentSession<C, N>
where
    C: Client,
    N: Namespace + Send + Sync,
{
    /// Entered subsegment.
    Entered {
        /// X-Ray client.
        client: C,
        /// X-Amzn-Trace-Id header.
        header: Header,
        /// Subsegment.
        subsegment: Subsegment,
        /// Namespace.
        namespace: N,
    },
    /// Subsegment of a trace the upstream did not sample: nothing is recorded, and the
    /// header still carries the upstream decision to downstream calls.
    Unsampled {
        /// X-Amzn-Trace-Id header.
        header: Header,
    },
    /// Failed subsegment.
    Failed,
}

impl<C, N> SubsegmentSession<C, N>
where
    C: Client,
    N: Namespace + Send + Sync,
{
    pub(crate) fn new(client: C, header: &Header, namespace: N, name_prefix: &str) -> Self {
        // the upstream decision binds this process: an unsampled trace records nothing here,
        // but downstream services still receive the decision rather than making their own.
        if header.sampling_decision == SamplingDecision::NotSampled {
            return Self::Unsampled { header: header.clone() };
        }
        let mut subsegment = Subsegment::begin(
            header.trace_id.clone(),
            header.parent_id.clone(),
            namespace.name(name_prefix),
        );
        namespace.update_subsegment(&mut subsegment);
        match client.send(&subsegment) {
            Ok(_) => Self::Entered {
                client,
                header: header.with_parent_id(subsegment.id.clone()),
                subsegment,
                namespace,
            },
            Err(_) => Self::Failed,
        }
    }

    pub(crate) fn failed() -> Self {
        Self::Failed
    }

    /// Returns the `x-amzn-trace-id` header value.
    pub fn x_amzn_trace_id(&self) -> Option<String> {
        match self {
            Self::Entered { header, .. } | Self::Unsampled { header } => Some(header.to_string()),
            Self::Failed => None,
        }
    }

    /// Returns the namespace as a mutable reference.
    pub fn namespace_mut(&mut self) -> Option<&mut N> {
        match self {
            Self::Entered { namespace, .. } => Some(namespace),
            Self::Unsampled { .. } | Self::Failed => None,
        }
    }
}

impl<C, N> Drop for SubsegmentSession<C, N>
where
    C: Client,
    N: Namespace + Send + Sync,
{
    fn drop(&mut self) {
        match self {
            Self::Entered {
                client,
                subsegment,
                namespace,
                ..
            } => {
                subsegment.end();
                namespace.update_subsegment(subsegment);
                let _ = client
                    .send(subsegment)
                    .map_err(|e| eprintln!("failed to end subsegment: {e}"));
            }
            Self::Unsampled { .. } | Self::Failed => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{AwsNamespace, Context, SubsegmentContext};

    use super::*;

    /// A client that keeps every document it is sent.
    #[derive(Clone, Debug, Default)]
    struct Recorder(Arc<Mutex<Vec<serde_json::Value>>>);

    impl Client for Recorder {
        fn send<S: serde::Serialize>(&self, data: &S) -> crate::Result<()> {
            self.0.lock().unwrap().push(serde_json::to_value(data)?);
            Ok(())
        }
    }

    fn header(sampled: &str) -> Header {
        format!("Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled={sampled}").parse().unwrap()
    }

    #[test]
    fn a_sampled_subsegment_is_recorded_and_parents_downstream_calls() {
        let client = Recorder::default();
        let session = SubsegmentContext::with_header(client.clone(), header("1")).enter_subsegment(AwsNamespace::new("S3", "GetObject"));
        let trace_header = session.x_amzn_trace_id().unwrap();
        drop(session);

        let documents = client.0.lock().unwrap();
        assert_eq!(2, documents.len(), "begin and end: {documents:#?}");
        let id = documents[0]["id"].as_str().unwrap();
        assert!(trace_header.contains(&format!("Parent={id}")), "{trace_header}");
        assert!(trace_header.contains("Sampled=1"), "{trace_header}");
    }

    #[test]
    fn an_unsampled_subsegment_records_nothing_and_propagates_the_decision() {
        let client = Recorder::default();
        let mut session = SubsegmentContext::with_header(client.clone(), header("0")).enter_subsegment(AwsNamespace::new("S3", "GetObject"));
        assert!(session.namespace_mut().is_none());
        let trace_header = session.x_amzn_trace_id().unwrap();
        drop(session);

        assert!(client.0.lock().unwrap().is_empty());
        assert_eq!(header("0").to_string(), trace_header);
    }
}
