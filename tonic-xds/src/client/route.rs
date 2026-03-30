use crate::common::async_util::BoxFuture;
use crate::xds::routing::RoutingError;
use http::Request;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{BoxError, Layer, Service};

/// Represents the input for routing decisions.
#[allow(dead_code)]
pub(crate) struct RouteInput<'a> {
    /// The authority (host) of the request URI.
    pub authority: &'a str,
    /// The HTTP headers of the request. These can be used for header-based routing decisions.
    pub headers: &'a http::HeaderMap,
}

/// Represents the routing decision made by the routing layer.
#[derive(Clone, Debug)]
pub(crate) struct RouteDecision {
    /// The name of the cluster to which the request should be routed.
    pub cluster: String,
}

/// Trait for routing requests to clusters.
///
/// Implementations resolve a request's authority and headers into a target
/// cluster name. The xDS-backed implementation is
/// [`XdsRouter`](crate::xds::routing::XdsRouter).
pub(crate) trait Router: Send + Sync + 'static {
    fn route(&self, input: &RouteInput<'_>) -> BoxFuture<Result<RouteDecision, RoutingError>>;
}

/// Tower service for routing requests to the appropriate cluster.
/// Attaches routing decision as [`RouteDecision`] to the request extensions.
/// The [`RouteDecision`] will be used by the `XdsLbService` to identify the
/// cluster to which the request should be routed.
#[derive(Clone)]
pub(crate) struct XdsRoutingService<S> {
    /// The inner Tower service to which the request will be forwarded after routing decision is made.
    inner: S,
    /// The router used to make routing decisions based on the request.
    router: Arc<dyn Router>,
}

impl<S, B> Service<Request<B>> for XdsRoutingService<S>
where
    S: Service<Request<B>, Error: Into<BoxError>> + Clone + Send + 'static,
    B: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let router = self.router.clone();
        let mut inner_service = self.inner.clone();
        Box::pin(async move {
            let authority = request
                .uri()
                .authority()
                .map_or("", http::uri::Authority::as_str);
            let headers = &request.headers();
            let route_input = RouteInput { authority, headers };
            let route_decision = router.route(&route_input).await?;
            request.extensions_mut().insert(route_decision);
            inner_service.call(request).await.map_err(Into::into)
        })
    }
}

/// Tower layer for routing requests to the appropriate cluster.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct XdsRoutingLayer {
    router: Arc<dyn Router>,
}

impl XdsRoutingLayer {
    /// Creates a new `XdsRoutingLayer` with the given [`Router`].
    #[allow(dead_code)]
    pub(crate) fn new(router: Arc<dyn Router>) -> Self {
        Self { router }
    }
}

impl<S> Layer<S> for XdsRoutingLayer {
    type Service = XdsRoutingService<S>;

    fn layer(&self, service: S) -> Self::Service {
        XdsRoutingService {
            inner: service,
            router: self.router.clone(),
        }
    }
}
