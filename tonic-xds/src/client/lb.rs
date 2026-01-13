use futures::future::BoxFuture;
use http::Request;
use tonic::client::GrpcService;
use std::future::Future;
use std::task::{Context, Poll};
use std::pin::Pin;
use std::sync::Arc;
use tower::{discover::Discover, load::Load, BoxError, Service};
use crate::client::cluster::ClusterClientRegistry;
use crate::xds::route::RouteDecision;
use crate::xds::xds_manager::XdsManager;
use tower::ServiceExt;

/// Errors that can occur during load balancing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LoadBalancingError {
    #[error("No routing decision extension from the routing layer available")]
    NoRoutingDecision,
}

/// A Tower service that performs load balancing based on routing decisions and xDS configuration.
pub(crate) struct XdsLbService<Req, E, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    cluster_registry: Arc<ClusterClientRegistry<Req, S::Response>>,
    xds_manager: Arc<dyn XdsManager<E, S>>,
}

impl<Req, E, S> XdsLbService<Req, E, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    /// Creates a new XdsLbService with the given cluster registry and xDS manager.
    pub(crate) fn new(
        cluster_registry: Arc<ClusterClientRegistry<Req, S::Response>>,
        xds_manager: Arc<dyn XdsManager<E, S>>,
    ) -> Self {
        Self {
            cluster_registry,
            xds_manager,
        }
    }
}

// Manual Clone implementation - derive doesn't work with dyn trait bounds
impl<Req, E, S> Clone for XdsLbService<Req, E, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            cluster_registry: self.cluster_registry.clone(),
            xds_manager: self.xds_manager.clone(),
        }
    }
}

impl<B, E, S> Service<Request<B>> for XdsLbService<Request<B>, E, S>
where
    Request<B>: Send + 'static,
    S::Response: Send + 'static,
    E: std::hash::Hash + Eq + Clone + Send + 'static,
    S: Service<Request<B>> + Load + Send + 'static,
    S::Response: Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send,
    <S as tower::load::Load>::Metric: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Ideally we expose the channel readiness here, but because the channel is specific to a cluster,
        // which may change per request, and we don't have request here, we check for readiness in call() instead.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let Some(routing_decision) = request.extensions().get::<RouteDecision>().cloned() else {
            return Box::pin(async move { Err(LoadBalancingError::NoRoutingDecision.into()) });
        };

        let cluster_client = self
            .cluster_registry
            .get_cluster(&routing_decision.cluster, || {
                self.xds_manager
                    .discover_cluster(&routing_decision.cluster)
            });

        let mut channel = cluster_client.channel();

        Box::pin(async move {
            // This will block until the first endpoint is available.
            channel.ready().await?;
            channel.call(request).await
        })
    }
}