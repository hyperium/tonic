//! XDS cluster load-balancing service for the `tonic-xds-lb` path.
//!
//! [`XdsLoadBalanceService`] is the analogue of the `tower-lb`
//! `XdsLbService`: it reads the [`RouteDecision`] attached by the routing
//! layer, looks up (or lazily builds) the target cluster's [`LoadBalancer`],
//! and dispatches the request to it.
//!
//! Each cluster's `LoadBalancer` is not `Clone` and needs `&mut self` to
//! serve, so it is wrapped in a [`tower::buffer::Buffer`] — giving a cheap,
//! cloneable handle shared across concurrent requests. The buffer maps the
//! LB's [`LbError`](crate::client::loadbalance::errors::LbError) to
//! [`BoxError`] automatically.
//!
//! TODO: only the power-of-two-choices picker ([`P2cPicker`]) is enabled;
//! outlier detection ([`OutlierDetectionConfig::default`]) and other pickers (e.g. ring-hash)
//! will be wired in by follow-ups.

use std::sync::Arc;
use std::task::{Context, Poll};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use http::{Request, Response};
use tonic::body::Body as TonicBody;
use tonic::transport::Channel;
use tower::buffer::Buffer;
use tower::{BoxError, Service, ServiceExt};

use crate::client::endpoint::EndpointChannel;
use crate::client::loadbalance::channel_state::ReadyChannel;
use crate::client::loadbalance::loadbalancer::{LbFuture, LoadBalancer};
use crate::client::loadbalance::pickers::ChannelPicker;
use crate::client::loadbalance::pickers::p2c::P2cPicker;
use crate::client::route::RouteDecision;
use crate::common::async_util::BoxFuture;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;
use crate::xds::lb_discovery::{XdsLbConnector, discover_endpoints};
use crate::xds::resource::outlier_detection::OutlierDetectionConfig;

/// Buffer capacity between callers and a cluster's `LoadBalancer` worker.
const DEFAULT_BUFFER_CAPACITY: usize = 1024;

/// The request type flowing into the LB layer.
type LbRequest = Request<TonicBody>;
/// The response type produced by an endpoint channel.
type LbResponse = Response<TonicBody>;

/// A cloneable handle to one cluster's `LoadBalancer`, buffered so concurrent
/// callers share a single balancer. `Buffer`'s second type parameter is the
/// wrapped service's future — here the `LoadBalancer`'s [`LbFuture`].
type ClusterChannel = Buffer<LbRequest, LbFuture<LbResponse>>;

/// Error returned when a request reaches the LB layer without a routing
/// decision (i.e. the routing layer did not run or produced nothing).
#[derive(Debug, thiserror::Error)]
#[error("no routing decision extension from the routing layer available")]
struct NoRoutingDecision;

/// Registry of per-cluster [`LoadBalancer`]s, built lazily on first use.
struct ClusterLbRegistry {
    cache: Arc<XdsCache>,
    #[cfg(feature = "_tls-any")]
    cert_provider_registry: Arc<CertProviderRegistry>,
    clusters: DashMap<String, ClusterChannel>,
}

impl ClusterLbRegistry {
    /// Returns a cloneable channel to the cluster's balancer, building it on
    /// first access.
    fn cluster_channel(&self, cluster_name: &str) -> ClusterChannel {
        self.clusters
            .entry(cluster_name.to_string())
            .or_insert_with(|| self.build_cluster_channel(cluster_name))
            .clone()
    }

    /// Constructs a fresh `LoadBalancer` for `cluster_name` and wraps it in a
    /// buffer. Discovery yields idle endpoints; [`XdsLbConnector`] resolves the
    /// cluster's CDS security config lazily inside `connect`.
    fn build_cluster_channel(&self, cluster_name: &str) -> ClusterChannel {
        let discover = discover_endpoints(&self.cache, cluster_name);
        let connector = Arc::new(XdsLbConnector::new(
            self.cache.clone(),
            cluster_name.to_string(),
            #[cfg(feature = "_tls-any")]
            self.cert_provider_registry.clone(),
        ));
        let picker: Arc<
            dyn ChannelPicker<ReadyChannel<EndpointChannel<Channel>>, LbRequest> + Send + Sync,
        > = Arc::new(P2cPicker);
        let config = Arc::new(ArcSwap::from_pointee(OutlierDetectionConfig::default()));
        let lb = LoadBalancer::new(discover, connector, picker, config);
        Buffer::new(lb, DEFAULT_BUFFER_CAPACITY)
    }
}

/// Tower service that routes each request to its cluster's `LoadBalancer`.
#[derive(Clone)]
pub(crate) struct XdsLoadBalanceService {
    registry: Arc<ClusterLbRegistry>,
}

impl XdsLoadBalanceService {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(
        cache: Arc<XdsCache>,
        cert_provider_registry: Arc<CertProviderRegistry>,
    ) -> Self {
        Self {
            registry: Arc::new(ClusterLbRegistry {
                cache,
                cert_provider_registry,
                clusters: DashMap::new(),
            }),
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(cache: Arc<XdsCache>) -> Self {
        Self {
            registry: Arc::new(ClusterLbRegistry {
                cache,
                clusters: DashMap::new(),
            }),
        }
    }
}

impl Service<LbRequest> for XdsLoadBalanceService {
    type Response = LbResponse;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The target cluster is decided per-request by the routing layer, so
        // readiness cannot be determined without the request.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: LbRequest) -> Self::Future {
        let Some(decision) = request.extensions().get::<RouteDecision>().cloned() else {
            return Box::pin(async move { Err(BoxError::from(NoRoutingDecision)) });
        };

        let mut channel = self.registry.cluster_channel(&decision.cluster);

        Box::pin(async move {
            // Blocks until the balancer has a ready endpoint.
            channel.ready().await?;
            channel.call(request).await
        })
    }
}
