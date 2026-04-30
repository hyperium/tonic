//! xDS-backed [`ClusterDiscovery`] implementation.
//!
//! Bridges [`XdsCache`] endpoint watches and [`EndpointManager`] diffing
//! to provide the [`ClusterDiscovery`] trait required by [`XdsLbService`].

use std::sync::Arc;

use tonic::transport::{Channel, Endpoint};

use crate::client::endpoint::{EndpointAddress, EndpointChannel};
use crate::client::lb::{BoxDiscover, ClusterDiscovery};
use crate::xds::cache::XdsCache;
use crate::xds::endpoint_manager::EndpointManager;

/// Shared connector function that creates endpoint services from addresses.
// TODO: Refactor to a trait when adding TLS support (A29). A trait can carry
// configuration (TLS settings, timeouts) and be shared across EndpointManager,
// ClusterDiscovery, and LB reconnect logic.
pub(crate) type EndpointConnector =
    Arc<dyn Fn(&EndpointAddress) -> EndpointChannel<Channel> + Send + Sync>;

/// xDS-backed cluster discovery that resolves cluster names into endpoint
/// change streams by watching the [`XdsCache`].
pub(crate) struct XdsClusterDiscovery {
    cache: Arc<XdsCache>,
    endpoint_manager: EndpointManager<EndpointChannel<Channel>>,
}

impl XdsClusterDiscovery {
    /// Creates a new `XdsClusterDiscovery`.
    pub(crate) fn new(cache: Arc<XdsCache>, connector: EndpointConnector) -> Self {
        Self {
            cache,
            endpoint_manager: EndpointManager::new(connector),
        }
    }
}

impl ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>> for XdsClusterDiscovery {
    fn discover_cluster(
        &self,
        cluster_name: &str,
    ) -> BoxDiscover<EndpointAddress, EndpointChannel<Channel>> {
        let watch = self.cache.watch_endpoints(cluster_name);
        self.endpoint_manager.discover_endpoints(watch)
    }
}

/// Default connector that creates a lazily-connected [`EndpointChannel`] for
/// each endpoint address.
///
/// Uses insecure (plaintext) connections.
// TODO(PR2/A29): Replace this with a TLS-aware connector that receives the
// CertProviderRegistry and per-cluster UpstreamTlsContext (from ClusterResource).
// When a cluster has transport_socket configured, the connector should:
//   1. Look up root + identity cert provider instances from the registry
//   2. Build ClientTlsConfig with the fetched CertificateData
//   3. Apply SAN matching for server authorization
//   4. Use connect() instead of connect_lazy() for TLS handshake
pub(crate) fn default_endpoint_connector(addr: &EndpointAddress) -> EndpointChannel<Channel> {
    let uri = format!("http://{addr}");
    // Safety: EndpointAddress only holds validated Ipv4/Ipv6/Hostname + u16 port,
    // and its Display impl produces "ip:port" or "hostname:port". Prefixing with
    // "http://" always yields a valid URI, so from_shared cannot fail here.
    let channel = Endpoint::from_shared(uri)
        .expect("EndpointAddress Display guarantees valid URI")
        .connect_lazy();
    EndpointChannel::new(channel)
}
