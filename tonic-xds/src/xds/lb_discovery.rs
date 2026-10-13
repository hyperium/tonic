//! xDS-backed discovery for the `tonic-xds-lb` load balancer.
//!
//! Unlike the `tower-lb` path (which connects inside discovery and yields
//! ready [`EndpointChannel`]s), the in-crate `LoadBalancer` manages the
//! connection lifecycle itself. So this module provides two decoupled pieces:
//!
//! 1. [`discover_endpoints`] — diffs EDS snapshots into
//!    `Change<EndpointAddress, IdleChannel>` events (addresses only; no
//!    connection is established here).
//! 2. [`XdsLbConnector`] — the [`Connector`] the `LoadBalancer` calls to turn
//!    an address into a live channel. Because [`Connector::connect`] returns a
//!    future, the connector is handed to the LB synchronously and resolves the
//!    cluster's CDS security config *inside* that future via
//!    [`build_connector`]. If CDS has not arrived yet (or a CDS update fails
//!    validation), the connect future parks until a valid config is available.
//! 
//! TODO: handle the case where the cluster is removed from the cache while a connect future is pending.
//! Currently, the future will park forever, it should instead try to re-establish the cluster watch and
//! only fail-fast when the client is closed.

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tower::BoxError;
use tower::discover::Change;

use crate::client::endpoint::{Connector, EndpointAddress, EndpointChannel};
use crate::client::loadbalance::channel_state::IdleChannel;
use crate::common::async_util::BoxFuture;
use crate::xds::cache::{CacheWatch, XdsCache};
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;
use crate::xds::connector::build_connector;
use crate::xds::resource::EndpointsResource;

/// Buffer capacity for the endpoint change channel between the diff loop and
/// the load balancer's `Discover`.
const ENDPOINT_CHANNEL_CAPACITY: usize = 64;

/// A pinned, boxed stream of endpoint changes, consumed by the `LoadBalancer`
/// through Tower's `Discover` blanket impl. Each inserted endpoint is carried
/// as an [`IdleChannel`] — the initial state of the channel state machine.
pub(crate) type EndpointDiscover =
    Pin<Box<dyn Stream<Item = Result<Change<EndpointAddress, IdleChannel>, BoxError>> + Send>>;

// Compile-time assertion that `EndpointDiscover` satisfies the bounds the
// `LoadBalancer` requires from its discovery.
const _: fn() = || {
    fn assert_discover<D>()
    where
        D: tower::discover::Discover<Key = EndpointAddress, Service = IdleChannel> + Unpin,
    {
    }
    assert_discover::<EndpointDiscover>();
};

/// Returns a stream of endpoint changes for a cluster.
///
/// Diffs each EDS snapshot against the previous set of healthy endpoints,
/// emitting `Change::Insert` (as a not-yet-connected [`IdleChannel`]) for new
/// endpoints and `Change::Remove` for gone ones.
pub(crate) fn discover_endpoints(cache: &Arc<XdsCache>, cluster_name: &str) -> EndpointDiscover {
    let (tx, rx) = mpsc::channel(ENDPOINT_CHANNEL_CAPACITY);
    tokio::spawn(diff_loop(cache.watch_endpoints(cluster_name), tx));
    Box::pin(ReceiverStream::new(rx))
}

/// Background task: watches EDS snapshots and emits incremental endpoint
/// changes. Exits when the cache watch closes or the receiver is dropped.
async fn diff_loop(
    mut watch: CacheWatch<EndpointsResource>,
    tx: mpsc::Sender<Result<Change<EndpointAddress, IdleChannel>, BoxError>>,
) {
    let mut active: HashSet<EndpointAddress> = HashSet::new();

    while let Some(endpoints) = watch.next().await {
        let new_set: HashSet<EndpointAddress> = endpoints
            .healthy_endpoints()
            .map(|ep| ep.address.clone())
            .collect();

        for added in new_set.difference(&active) {
            let change = Change::Insert(added.clone(), IdleChannel::new(added.clone()));
            if tx.send(Ok(change)).await.is_err() {
                return;
            }
        }

        for removed in active.difference(&new_set) {
            if tx.send(Ok(Change::Remove(removed.clone()))).await.is_err() {
                return;
            }
        }

        active = new_set;
    }
}

/// The [`Connector`] used by the `tonic-xds-lb` `LoadBalancer`.
///
/// Resolves the cluster's CDS security config lazily, inside the connect
/// future: on each `connect`, it reads the current
/// [`ClusterResource`](crate::xds::resource::ClusterResource) from
/// the cache and builds the appropriate plaintext/TLS connector via
/// [`build_connector`]. This lets the connector be constructed synchronously
/// (before CDS/EDS resolve) while the returned future waits for a valid
/// config.
pub(crate) struct XdsLbConnector {
    cache: Arc<XdsCache>,
    cluster_name: String,
    #[cfg(feature = "_tls-any")]
    cert_provider_registry: Arc<CertProviderRegistry>,
}

impl XdsLbConnector {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(
        cache: Arc<XdsCache>,
        cluster_name: String,
        registry: Arc<CertProviderRegistry>,
    ) -> Self {
        Self {
            cache,
            cluster_name,
            cert_provider_registry: registry,
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(cache: Arc<XdsCache>, cluster_name: String) -> Self {
        Self {
            cache,
            cluster_name,
        }
    }
}

impl Connector for XdsLbConnector {
    type Service = EndpointChannel<Channel>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        let mut cluster_watch = self.cache.watch_cluster(&self.cluster_name);
        let cluster_name = self.cluster_name.clone();
        let addr = addr.clone();
        #[cfg(feature = "_tls-any")]
        let registry = self.cert_provider_registry.clone();

        Box::pin(async move {
            loop {
                let Some(cluster) = cluster_watch.next().await else {
                    // Cluster removed from cache: no connector can be built.
                    // The LB drops this future when the endpoint leaves
                    // discovery, so park until then.
                    return std::future::pending().await;
                };
                match build_connector(
                    &cluster,
                    #[cfg(feature = "_tls-any")]
                    &registry,
                ) {
                    Ok(connector) => return connector.connect(&addr).await,
                    Err(e) => tracing::warn!(
                        cluster = %cluster_name,
                        error = %e,
                        "CDS connector build failed; awaiting next CDS update",
                    ),
                }
            }
        })
    }
}
