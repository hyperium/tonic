use crate::client::cluster::ClusterClientRegistry;
use crate::client::route::RouteDecision;
use crate::common::async_util::BoxFuture;
use http::Request;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::ServiceExt;
use tower::{BoxError, Service, discover::Change, load::Load};

/// A pinned, boxed stream of endpoint changes for Tower's `Discover`-based
/// load balancers.
pub(crate) type BoxDiscover<Endpoint, S> =
    Pin<Box<dyn futures_core::Stream<Item = Result<Change<Endpoint, S>, BoxError>> + Send>>;

/// Trait for discovering cluster endpoints.
///
/// Implementations resolve a cluster name into a stream of endpoint changes
/// (`Change::Insert` / `Change::Remove`). The xDS-backed implementation is
/// [`XdsClusterDiscovery`](crate::xds::cluster_discovery::XdsClusterDiscovery).
pub(crate) trait ClusterDiscovery<Endpoint, S>: Send + Sync + 'static {
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<Endpoint, S>;
}

/// Errors that can occur during load balancing.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum LoadBalancingError {
    #[error("No routing decision extension from the routing layer available")]
    NoRoutingDecision,
}

/// A Tower Service that performs load balancing based on routing decisions.
pub(crate) struct XdsLbService<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    cluster_registry: Arc<ClusterClientRegistry<Req, S::Response>>,
    cluster_discovery: Arc<dyn ClusterDiscovery<Endpoint, S>>,
}

impl<Req, Endpoint, S> XdsLbService<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    /// Creates a new `XdsLbService` with the given cluster client registry and cluster discovery.
    #[allow(dead_code)]
    pub(crate) fn new(
        cluster_registry: Arc<ClusterClientRegistry<Req, S::Response>>,
        cluster_discovery: Arc<dyn ClusterDiscovery<Endpoint, S>>,
    ) -> Self {
        Self {
            cluster_registry,
            cluster_discovery,
        }
    }
}

impl<Req, Endpoint, S> Clone for XdsLbService<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            cluster_registry: self.cluster_registry.clone(),
            cluster_discovery: self.cluster_discovery.clone(),
        }
    }
}

impl<B, Endpoint, S> Service<Request<B>> for XdsLbService<Request<B>, Endpoint, S>
where
    Request<B>: Send + 'static,
    S::Response: Send + 'static,
    Endpoint: std::hash::Hash + Eq + Clone + Send + 'static,
    S: Service<Request<B>> + Load + Send + 'static,
    S::Response: Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send,
    <S as tower::load::Load>::Metric: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Under xDS, the destination cluster is decided by the routing layer, which takes
        // the request as an input. Therefore, we cannot determine readiness without
        // knowing the target cluster, which is tied to the request.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        // Extract the routing decision from the request extensions.
        let Some(routing_decision) = request.extensions().get::<RouteDecision>().cloned() else {
            return Box::pin(async move { Err(LoadBalancingError::NoRoutingDecision.into()) });
        };

        // Get or create the cluster client for the target xDS cluster.
        let cluster_client = self
            .cluster_registry
            .get_cluster(&routing_decision.cluster, || {
                self.cluster_discovery
                    .discover_cluster(&routing_decision.cluster)
            });

        // Get the transport channel for the target xDS cluster.
        // The actual load-balancing will be performed by the cluster's balancer.
        let mut channel = cluster_client.channel();

        Box::pin(async move {
            // This will block until the first endpoint is available.
            channel.ready().await?;
            channel.call(request).await
        })
    }
}
