//! Configuration for the xDS client.

use std::time::Duration;

use crate::message::Node;

/// Configuration for the xDS client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Node identification sent to the xDS server.
    ///
    /// Default: None.
    pub node: Option<Node>,

    /// Timeout for "resource does not exist" detection.
    ///
    /// If a subscribed resource is not received within this duration,
    /// watchers are notified with a `ResourceDoesNotExist` error.
    ///
    /// Default: 15 seconds (per xDS spec).
    pub resource_timeout: Duration,

    /// Initial backoff duration for reconnection attempts.
    ///
    /// Default: 1 second.
    pub initial_backoff: Duration,

    /// Maximum backoff duration for reconnection attempts.
    ///
    /// Default: 30 seconds.
    pub max_backoff: Duration,

    /// Multiplier for exponential backoff.
    ///
    /// After each failed connection attempt, the backoff duration is multiplied
    /// by this value, up to `max_backoff`.
    ///
    /// Default: 2.0.
    pub backoff_multiplier: f64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            node: None,
            resource_timeout: Duration::from_secs(15),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

impl ClientConfig {
    /// Create a new configuration with the given node identification.
    pub fn new(node: Node) -> Self {
        Self {
            node: Some(node),
            ..Default::default()
        }
    }

    /// Create a new configuration with a node ID.
    pub fn with_node_id(id: impl Into<String>) -> Self {
        Self::new(Node {
            id: id.into(),
            ..Default::default()
        })
    }

    /// Set the node identification.
    pub fn node(mut self, node: Node) -> Self {
        self.node = Some(node);
        self
    }

    /// Set the resource timeout.
    pub fn resource_timeout(mut self, timeout: Duration) -> Self {
        self.resource_timeout = timeout;
        self
    }

    /// Set the initial backoff duration.
    pub fn initial_backoff(mut self, duration: Duration) -> Self {
        self.initial_backoff = duration;
        self
    }

    /// Set the maximum backoff duration.
    pub fn max_backoff(mut self, duration: Duration) -> Self {
        self.max_backoff = duration;
        self
    }

    /// Set the backoff multiplier.
    pub fn backoff_multiplier(mut self, multiplier: f64) -> Self {
        self.backoff_multiplier = multiplier;
        self
    }
}
