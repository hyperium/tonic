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

use crate::common::async_util::BoxFuture;
use dashmap::DashMap;
use http::{Request, Response};
use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tonic::body::Body as TonicBody;
use tower::{
    BoxError, Service, balance::p2c::Balance, buffer::Buffer, discover::Discover, load::Load,
};

type RespFut<Resp> = BoxFuture<Result<Resp, BoxError>>;

const DEFAULT_BUFFER_CAPACITY: usize = 1024;

/// `ClusterBalancer` is responsible for managing load balancing requests across multiple channels.
/// Currently, `ClusterBalancer` leverges `tower::balance::p2c` for doing P2C load balancing. In the future, we will
/// support more load balancing strategies as needed.
pub(crate) struct ClusterBalancer<D, Req>
where
    D: Discover,
    D::Key: Hash,
{
    balancer: Balance<D, Req>,
}

impl<D, Req> ClusterBalancer<D, Req>
where
    D: Discover,
    D::Key: Hash,
    D::Service: Service<Req>,
    <D::Service as Service<Req>>::Error: Into<BoxError>,
{
    /// Creates a new `ClusterBalancer` with provided service discovery.
    pub(crate) fn new(discover: D) -> Self {
        Self {
            balancer: Balance::new(discover),
        }
    }

    /// Returns the number of endpoints currently tracked by the balancer.
    /// This can be useful for monitoring and debugging purposes.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.balancer.len()
    }
}

impl<D, Req> Service<Req> for ClusterBalancer<D, Req>
where
    D: Discover + Unpin,
    D::Key: Hash + Clone,
    D::Error: Into<BoxError>,
    D::Service: Service<Req> + Load,
    <D::Service as Load>::Metric: std::fmt::Debug,
    <D::Service as Service<Req>>::Error: Into<BoxError> + 'static,
    <D::Service as Service<Req>>::Future: Send + 'static,
{
    type Response = <Balance<D, Req> as Service<Req>>::Response;
    type Error = <Balance<D, Req> as Service<Req>>::Error;
    type Future = RespFut<Self::Response>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.balancer.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        Box::pin(self.balancer.call(req))
    }
}

/// `ClusterChannel` is similar to `tonic::transport::Channel`, but is for load-balancing across all
/// the channels for a xDS Cluster.
/// `ClusterChannel` should be cloned to be used in multi-threaded environment. It leverages a `tower::Buffer` to
/// queue requests from multiple callers and behind the queue, it load-balances the requests across all
/// available channels by leveraging the inner `ClusterBalancer` object.
pub(crate) struct ClusterChannel<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    // The mpsc channel between callers and the actual pool of channels.
    svc: Buffer<Req, BoxFuture<Result<Resp, BoxError>>>,
}

impl<Req, Resp> Clone for ClusterChannel<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    fn clone(&self) -> Self {
        Self {
            svc: self.svc.clone(),
        }
    }
}

impl<Req, Resp> ClusterChannel<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Creates a channel that queues callers in front of `svc`, the service
    /// that load-balances across the cluster's endpoints.
    pub(crate) fn from_service<B>(svc: B) -> Self
    where
        B: Service<Req, Error = BoxError, Future = RespFut<Resp>> + Send + 'static,
    {
        let svc = Buffer::new(svc, DEFAULT_BUFFER_CAPACITY);
        Self { svc }
    }

    /// Creates a channel that P2C-balances across the endpoints yielded by
    /// `discover`.
    pub(crate) fn from_discover<D>(discover: D) -> Self
    where
        D: Discover + Unpin + Send + 'static,
        D::Key: Hash + Clone + Send,
        D::Error: Into<BoxError>,
        D::Service: Service<Req, Response = Resp> + Load + Send + 'static,
        <D::Service as Load>::Metric: Debug,
        <D::Service as Service<Req>>::Error: Into<BoxError>,
        <D::Service as Service<Req>>::Future: Send + 'static,
    {
        Self::from_service(ClusterBalancer::new(discover))
    }
}

/// Builds the load-balancing channel for a cluster, on first use.
///
/// The seam between the LB dispatch layer ([`XdsLbService`]) and the balancer
/// behind it: the layer resolves a request to a cluster name and asks the
/// factory for that cluster's channel, without knowing how endpoints are
/// discovered or balanced.
///
/// [`XdsLbService`]: crate::client::lb::XdsLbService
pub(crate) trait ClusterChannelFactory<Req, Resp>: Send + Sync + 'static
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Builds the channel for `cluster_name`.
    fn make(&self, cluster_name: &str) -> ClusterChannel<Req, Resp>;
}

impl<Req, Resp> Service<Req> for ClusterChannel<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    type Response = Resp;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.svc, cx).map_err(BoxError::from)
    }

    fn call(&mut self, request: Req) -> Self::Future {
        Box::pin(self.svc.call(request))
    }
}

/// `ClusterClient` manages channels that load-balance for a xDS cluster.
pub(crate) struct ClusterClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    name: String,
    channel: ClusterChannel<Req, Resp>,
}

impl Debug for ClusterClient<(), ()> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterClient")
            .field("name", &self.name)
            .finish()
    }
}

impl<Req, Resp> ClusterClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Creates a `ClusterClient` serving `name` through an already-built
    /// load-balancing channel.
    pub(crate) fn from_channel(name: String, channel: ClusterChannel<Req, Resp>) -> Self {
        Self { name, channel }
    }

    /// Returns a channel that can be used to send RPCs to the cluster.
    pub(crate) fn channel(&self) -> ClusterChannel<Req, Resp> {
        self.channel.clone()
    }

    /// Returns the name of the cluster.
    #[allow(dead_code)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// `ClusterRegistry` is the client registry for all xDS clusters.
/// The xDS Tower service implementations uses this to get the client for a specific cluster.
pub(crate) struct ClusterClientRegistry<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    registry: DashMap<String, Arc<ClusterClient<Req, Resp>>>,
}

impl<Req, Resp> ClusterClientRegistry<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Creates a new `ClusterClientRegistry`.
    pub(crate) fn new() -> Self {
        Self {
            registry: DashMap::new(),
        }
    }
    /// Get the client of a cluster, building its channel on first use.
    pub(crate) fn get_cluster<F>(&self, key: &str, make_channel: F) -> Arc<ClusterClient<Req, Resp>>
    where
        F: FnOnce() -> ClusterChannel<Req, Resp>,
    {
        self.registry
            .entry(key.to_string())
            .or_insert_with(|| {
                Arc::new(ClusterClient::from_channel(key.to_string(), make_channel()))
            })
            .clone()
    }
}

impl<Req, Resp> Default for ClusterClientRegistry<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// A type erased registry for Tonic clients.
/// This will be used by the xDS Tower Service implementations to get the client for a specific Tonic xDS cluster.
#[allow(dead_code)]
pub(crate) type ClusterClientRegistryGrpc =
    ClusterClientRegistry<Request<TonicBody>, Response<TonicBody>>;
