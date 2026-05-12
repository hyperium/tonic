//! xDS-backed [`ClusterDiscovery`] implementation.
//!
//! Per cluster, [`XdsClusterDiscovery::discover_cluster`] spawns a task that
//! drives two concurrent watches:
//!
//! 1. The cluster resource watch — produces a fresh [`Connector`] on each
//!    CDS update (e.g. when `transport_socket` changes). The connector is
//!    held inside a [`ConnectorSwap`] so the diff loop reads the latest
//!    snapshot per endpoint connection.
//! 2. The endpoint watch — produces `Change::Insert` / `Change::Remove`
//!    events forwarded to the LB layer.
//!
//! On a CDS update whose security config fails validation, the previous
//! connector is kept and a warning is logged.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};

use crate::client::endpoint::{Connector, EndpointAddress, EndpointChannel};
use crate::client::lb::{BoxDiscover, ClusterDiscovery};
use crate::common::async_util::BoxFuture;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;
use crate::xds::endpoint_manager::{ConnectorSwap, EndpointManager};
use crate::xds::resource::ClusterResource;

/// Buffer capacity for the discovery channel between the spawned task and
/// Tower's LB layer.
const DISCOVER_CHANNEL_CAPACITY: usize = 64;

/// xDS-backed cluster discovery.
///
/// Resolves cluster names into endpoint change streams by watching the
/// [`XdsCache`]. Builds per-cluster [`Connector`]s based on the cluster's
/// [`ClusterSecurityConfig`] (if any) and the bootstrap-built
/// [`CertProviderRegistry`].
pub(crate) struct XdsClusterDiscovery {
    cache: Arc<XdsCache>,
    #[cfg(feature = "_tls-any")]
    cert_provider_registry: Arc<CertProviderRegistry>,
}

impl XdsClusterDiscovery {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(cache: Arc<XdsCache>, registry: Arc<CertProviderRegistry>) -> Self {
        Self {
            cache,
            cert_provider_registry: registry,
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(cache: Arc<XdsCache>) -> Self {
        Self { cache }
    }
}

impl ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>> for XdsClusterDiscovery {
    fn discover_cluster(
        &self,
        cluster_name: &str,
    ) -> BoxDiscover<EndpointAddress, EndpointChannel<Channel>> {
        let cache = self.cache.clone();
        let cluster_name = cluster_name.to_string();
        #[cfg(feature = "_tls-any")]
        let registry = self.cert_provider_registry.clone();

        let (tx, rx) = mpsc::channel(DISCOVER_CHANNEL_CAPACITY);

        tokio::spawn(async move {
            let mut cluster_watch = cache.watch_cluster(&cluster_name);

            let connector_swap: ConnectorSwap<EndpointChannel<Channel>> = loop {
                let Some(cluster) = cluster_watch.next().await else {
                    return;
                };
                let result = build_connector(
                    &cluster,
                    #[cfg(feature = "_tls-any")]
                    &registry,
                );
                match result {
                    Ok(c) => break Arc::new(ArcSwap::from_pointee(c)),
                    Err(e) => tracing::warn!(
                        cluster = %cluster_name,
                        error = %e,
                        "initial CDS update rejected; awaiting next update",
                    ),
                }
            };

            let manager = EndpointManager::new(Arc::clone(&connector_swap));
            let mut endpoints = manager.discover_endpoints(cache.watch_endpoints(&cluster_name));

            loop {
                tokio::select! {
                    Some(change) = endpoints.next() => {
                        if tx.send(change).await.is_err() {
                            return;
                        }
                    }
                    Some(cluster) = cluster_watch.next() => {
                        let result = build_connector(
                            &cluster,
                            #[cfg(feature = "_tls-any")]
                            &registry,
                        );
                        match result {
                            Ok(new) => connector_swap.store(Arc::new(new)),
                            Err(e) => tracing::warn!(
                                cluster = %cluster_name,
                                error = %e,
                                "CDS update rejected; keeping previous connector",
                            ),
                        }
                    }
                    else => return,
                }
            }
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

/// Build a [`Connector`] for the given cluster.
///
/// - `cluster.security == None` → [`PlaintextConnector`].
/// - `cluster.security == Some(_)` under a TLS feature → `TlsConnector`
///   (impl pending — see follow-up commit).
/// - `cluster.security == Some(_)` without a TLS feature → error.
fn build_connector(
    cluster: &ClusterResource,
    #[cfg(feature = "_tls-any")] _registry: &CertProviderRegistry,
) -> Result<Arc<dyn Connector<Service = EndpointChannel<Channel>> + Send + Sync>, ConnectorBuildError>
{
    match &cluster.security {
        None => Ok(Arc::new(PlaintextConnector)),
        #[cfg(feature = "_tls-any")]
        Some(_) => Err(ConnectorBuildError::TlsNotYetWired),
        #[cfg(not(feature = "_tls-any"))]
        Some(_) => Err(ConnectorBuildError::TlsFeatureMissing),
    }
}

/// Errors building a per-cluster [`Connector`] from a [`ClusterResource`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectorBuildError {
    /// Placeholder for the TLS connector path while the implementation is
    /// being wired up.
    // TODO: remove once `TlsConnector` lands in the follow-up commit.
    #[cfg(feature = "_tls-any")]
    #[error("TLS connector implementation is pending the follow-up commit")]
    TlsNotYetWired,
    /// The cluster requires TLS but the binary was built without a TLS
    /// crypto backend feature.
    #[cfg(not(feature = "_tls-any"))]
    #[error("cluster requires TLS but no TLS feature enabled (build with tls-ring or tls-aws-lc)")]
    TlsFeatureMissing,
}

/// Plaintext (non-TLS) [`Connector`] that produces a lazily-connected
/// `tonic::Channel` for each endpoint.
pub(crate) struct PlaintextConnector;

impl Connector for PlaintextConnector {
    type Service = EndpointChannel<Channel>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        // EndpointAddress only holds validated Ipv4/Ipv6/Hostname + u16 port,
        // and its Display impl produces "ip:port" or "hostname:port". Prefixing
        // with "http://" always yields a valid URI, so from_shared cannot fail.
        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .expect("EndpointAddress Display guarantees valid URI")
            .connect_lazy();
        let svc = EndpointChannel::new(channel);
        Box::pin(async move { svc })
    }
}
