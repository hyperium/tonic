//! Configuration for the xDS client.

use std::time::Duration;

use crate::client::retry::RetryPolicy;
use crate::message::Node;

/// Configuration for an xDS management server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ServerConfig {
    uri: String,
    // Future extensions per gRFC:
    // - `ignore_resource_deletion: bool` (gRFC A53)
    // - Server features / capabilities
    // - Per-server channel credentials config
}

impl ServerConfig {
    /// Create a new server configuration with the given URI.
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: uri.into() }
    }

    /// Returns the URI of the management server.
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

/// Default timeout for initial resource response (30 seconds per gRFC A57).
pub const DEFAULT_RESOURCE_INITIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for the xDS client.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClientConfig {
    /// Node identification sent to the xDS server.
    pub(crate) node: Node,

    /// Retry policy for connection attempts.
    ///
    /// Controls the backoff behavior when reconnecting to the xDS server.
    pub(crate) retry_policy: RetryPolicy,

    /// Priority-ordered list of xDS management servers.
    ///
    /// The client will attempt to connect to servers in order, falling back
    /// to the next server if the current one is unavailable (per gRFC A71).
    /// Index 0 has the highest priority.
    pub(crate) servers: Vec<ServerConfig>,

    /// Timeout for initial resource response (gRFC A57).
    ///
    /// If a watched resource is not received within this duration after the watch
    /// is registered, watchers receive a `ResourceDoesNotExist` error.
    ///
    /// Default: 30 seconds. Set to `None` to disable the timeout.
    pub(crate) resource_initial_timeout: Option<Duration>,
    // Future extensions:
    // - `authorities: HashMap<String, AuthorityConfig>` for xDS federation (gRFC A47)
    // - Locality / zone information for locality-aware routing
}

impl ClientConfig {
    /// Create a new configuration with a single server.
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
    /// let config = ClientConfig::new(node, "https://xds.example.com:443");
    /// ```
    pub fn new(node: Node, server_uri: impl Into<String>) -> Self {
        Self {
            node,
            retry_policy: RetryPolicy::default(),
            servers: vec![ServerConfig::new(server_uri)],
            resource_initial_timeout: Some(DEFAULT_RESOURCE_INITIAL_TIMEOUT),
        }
    }

    /// Create a new configuration with multiple servers for fallback.
    ///
    /// Servers are tried in order; index 0 has the highest priority.
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::{ClientConfig, Node, ServerConfig};
    ///
    /// let node = Node::new("grpc", "1.0");
    /// let config = ClientConfig::with_servers(node, vec![
    ///     ServerConfig::new("https://primary.xds.example.com:443"),
    ///     ServerConfig::new("https://backup.xds.example.com:443"),
    /// ]);
    /// ```
    pub fn with_servers(node: Node, servers: Vec<ServerConfig>) -> Self {
        Self {
            node,
            retry_policy: RetryPolicy::default(),
            servers,
            resource_initial_timeout: Some(DEFAULT_RESOURCE_INITIAL_TIMEOUT),
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
    /// let config = ClientConfig::new(node, "https://xds.example.com:443")
    ///     .with_retry_policy(policy);
    /// ```
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Set the timeout for initial resource response (gRFC A57).
    ///
    /// If a watched resource is not received within this duration after the watch
    /// is registered, watchers receive a `ResourceDoesNotExist` error.
    ///
    /// Set to `None` to disable the timeout.
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::{ClientConfig, Node};
    /// use std::time::Duration;
    ///
    /// let node = Node::new("grpc", "1.0");
    ///
    /// // Use a custom timeout
    /// let config = ClientConfig::new(node.clone(), "https://xds.example.com:443")
    ///     .with_resource_initial_timeout(Some(Duration::from_secs(60)));
    ///
    /// // Disable the timeout
    /// let config = ClientConfig::new(node, "https://xds.example.com:443")
    ///     .with_resource_initial_timeout(None);
    /// ```
    pub fn with_resource_initial_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.resource_initial_timeout = timeout;
        self
    }
}
