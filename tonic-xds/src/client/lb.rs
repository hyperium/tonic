/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use crate::client::cluster::{ClusterChannel, ClusterChannelFactory, ClusterClientRegistry};
use crate::client::route::RouteDecision;
use crate::common::async_util::BoxFuture;
use http::Request;
use std::hash::Hash;
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
pub(crate) struct XdsLbService<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    cluster_registry: Arc<ClusterClientRegistry<Req, Resp>>,
    cluster_factory: Arc<dyn ClusterChannelFactory<Req, Resp>>,
}

impl<Req, Resp> XdsLbService<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Creates a new `XdsLbService` with the given cluster client registry and cluster factory.
    pub(crate) fn new(
        cluster_registry: Arc<ClusterClientRegistry<Req, Resp>>,
        cluster_factory: Arc<dyn ClusterChannelFactory<Req, Resp>>,
    ) -> Self {
        Self {
            cluster_registry,
            cluster_factory,
        }
    }
}

impl<Req, Resp> Clone for XdsLbService<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    fn clone(&self) -> Self {
        Self {
            cluster_registry: self.cluster_registry.clone(),
            cluster_factory: self.cluster_factory.clone(),
        }
    }
}

impl<B, Resp> Service<Request<B>> for XdsLbService<Request<B>, Resp>
where
    Request<B>: Send + 'static,
    Resp: Send + 'static,
{
    type Response = Resp;
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
                self.cluster_factory.make(&routing_decision.cluster)
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

/// [`ClusterChannelFactory`] backed by a [`ClusterDiscovery`], balancing each
/// cluster's endpoints with Tower's P2C balancer.
pub(crate) struct TowerP2cClusterChannelFactory<Endpoint, S> {
    discovery: Arc<dyn ClusterDiscovery<Endpoint, S>>,
}

impl<Endpoint, S> TowerP2cClusterChannelFactory<Endpoint, S> {
    pub(crate) fn new(discovery: Arc<dyn ClusterDiscovery<Endpoint, S>>) -> Self {
        Self { discovery }
    }
}

impl<Req, Endpoint, S> ClusterChannelFactory<Req, S::Response>
    for TowerP2cClusterChannelFactory<Endpoint, S>
where
    Req: Send + 'static,
    Endpoint: Hash + Eq + Clone + Send + 'static,
    S: Service<Req> + Load + Send + 'static,
    S::Response: Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
    <S as Load>::Metric: std::fmt::Debug,
{
    fn make(&self, cluster_name: &str) -> ClusterChannel<Req, S::Response> {
        ClusterChannel::from_discover(self.discovery.discover_cluster(cluster_name))
    }
}
