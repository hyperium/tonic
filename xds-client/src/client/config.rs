//! Configuration for the xDS client.

use crate::client::retry::RetryPolicy;
use crate::message::Node;

/// Configuration for the xDS client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Node identification sent to the xDS server.
    pub node: Node,

    /// Retry policy for connection attempts.
    ///
    /// Controls the backoff behavior when reconnecting to the xDS server.
    pub retry_policy: RetryPolicy,
}

impl ClientConfig {
    /// Create a new configuration with the given node identification.
    ///
    /// Uses the default retry policy.
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::{ClientConfig, Node};
    ///
    /// let node = Node::new("grpc", "1.0")
    ///     .with_id("my-node")
    ///     .with_cluster("my-cluster");
    ///
    /// let config = ClientConfig::new(node);
    /// ```
    pub fn new(node: Node) -> Self {
        Self {
            node,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Set the retry policy.
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::{ClientConfig, Node, RetryPolicy};
    /// use std::time::Duration;
    ///
    /// let node = Node::new("grpc", "1.0");
    /// let policy = RetryPolicy::default()
    ///     .with_initial_backoff(Duration::from_millis(500)).unwrap()
    ///     .with_max_backoff(Duration::from_secs(60)).unwrap();
    ///
    /// let config = ClientConfig::new(node).with_retry_policy(policy);
    /// ```
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }
}
