use dashmap::DashMap;
use http::{Request, Response};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::ServiceExt;
use tonic::body::Body as TonicBody;
use tower::{balance::p2c::Balance, buffer::Buffer, discover::Discover, load::Load, Service, BoxError};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type RespFut<Resp> = Pin<Box<dyn Future<Output = Result<Resp, BoxError>> + Send>>;

const DEFAULT_BUFFER_CAPACITY: usize = 1024;

/// ClusterBalancer is responsible for managing load balancing requests across multiple channels.
/// Currently, ClusterBalancer leverges tower::balance::p2c for doing P2C load balancing. In the future, we will
/// swap it for a in-house implementation with more flexibility.
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
    /// Creates a new ClusterBalancer with provided service discovery.
    pub(crate) fn new(discover: D) -> Self {
        Self {
            balancer: Balance::new(discover),
        }
    }

    /// Returns the number of endpoints currently tracked by the balancer.
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

/// ClusterChannel is similar to tonic::transport::Channel, but is for load-balancing across all
/// the channels for a xDS Cluster.
/// ClusterChannel should be cloned to be used in multi-threaded environment. It leverages a tower::Buffer to
/// queue requests from multiple callers and behind the queue, it load-balances the requests across all
/// available channels by leveraging the inner ClusterBalancer object.
pub(crate) struct ClusterChannel<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    // The mpsc channel between callers and the actual pool of channels.
    svc: Buffer<Req, BoxFuture<'static, Result<Resp, BoxError>>>,
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
    /// Creates a new ClusterChannel with the given service and picker.
    pub(crate) fn from_balancer<B>(balancer: B, buffer_cap: usize) -> Self
    where
        B: Service<Req, Error = BoxError, Future = RespFut<Resp>> + Send + 'static,
    {
        let svc = Buffer::new(balancer, buffer_cap);
        Self { svc }
    }
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

// A type erased cluster channel for tonic clients.
/// See [ClusterChannel] for details.
pub(crate) type ClusterChannelGrpc = ClusterChannel<Request<TonicBody>, Response<TonicBody>>;

/// Cluster manages channels and load balancing for a xDS cluster.
pub(crate) struct ClusterClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    name: String,
    channel: ClusterChannel<Req, Resp>,
}

impl<Req, Resp> ClusterClient<Req, Resp>
where
    Req: Send + 'static,
    Resp: 'static,
{
    /// Creates a new ClusterClient with the given service and balancer.
    pub(crate) fn new<D>(name: String, discover: D) -> Self
    where
        D: Discover + Unpin + Send + 'static,
        D::Key: std::hash::Hash + Clone + Send,
        D::Error: Into<BoxError>,
        D::Service: Service<Req, Response = Resp> + Load + Send + 'static,
        <D::Service as Load>::Metric: std::fmt::Debug,
        <D::Service as Service<Req>>::Error: Into<BoxError>,
        <D::Service as Service<Req>>::Future: Send + 'static,
    {
        let balancer = ClusterBalancer::new(discover);
        let channel = ClusterChannel::from_balancer(balancer, DEFAULT_BUFFER_CAPACITY);
        Self { name, channel }
    }

    /// Returns a channel that can be used to send RPCs to the cluster.
    pub(crate) fn channel(&self) -> ClusterChannel<Req, Resp> {
        self.channel.clone()
    }

    /// Returns the name of the cluster.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// ClusterRegistry is the registry for all xDS clusters.
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
    /// Creates a new ClusterClientRegistry.
    pub(crate) fn new() -> Self {
        Self {
            registry: DashMap::new(),
        }
    }
    /// Get the client of a cluster with lazy discovery.
    /// Only calls discover_fn if the cluster is not already cached.
    /// This optimizes for the hot path where clusters are already cached.
    pub(crate) fn get_cluster<F, D>(&self, key: &str, discover_fn: F) -> Arc<ClusterClient<Req, Resp>>
    where
        F: FnOnce() -> D,
        D: Discover + Unpin + Send + 'static,
        D::Key: std::hash::Hash + Clone + Send,
        D::Error: Into<BoxError>,
        D::Service: Service<Req, Response = Resp> + Load + Send + 'static,
        <D::Service as Load>::Metric: std::fmt::Debug,
        <D::Service as Service<Req>>::Error: Into<BoxError>,
        <D::Service as Service<Req>>::Future: Send + 'static,
    {
        let client = self
            .registry
            .entry(key.to_string())
            .or_insert_with(|| {
                let name = key.to_string();
                let discover = discover_fn();
                Arc::new(ClusterClient::new(name, discover))
            })
            .clone();
        client
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

/// A type erased registry for tonic clients.
pub(crate) type ClusterClientRegistryGrpc = ClusterClientRegistry<Request<TonicBody>, Response<TonicBody>>;