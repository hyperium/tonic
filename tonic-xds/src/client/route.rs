use http::Request;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::pin::Pin;
use tower::{BoxError, Layer, Service};
use crate::xds::xds_manager::XdsManager;
use crate::xds::route::RouteInput;

/// Service wrapper for routing requests to the appropriate cluster based on the `RouteConfiguration`.
/// Attaches `RoutingDecision` to the request extensions.
/// The `RoutingDecision` will be used by the LB layer to load balance the request to the appropriate endpoint.
#[derive(Clone)]
pub(crate) struct XdsRoutingService<S, E, C> {
    inner: S,
    xds_manager: Arc<dyn XdsManager<E, C>>,
}

impl<S, B, E, C> Service<Request<B>> for XdsRoutingService<S, E, C>
where
    S: Service<Request<B>, Error = BoxError> + Clone + Send + 'static, B: Send + 'static,
    S::Future: Send + 'static,
    E: Send + 'static,
    C: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let xds_manager = self.xds_manager.clone();
        let mut inner_service = self.inner.clone();
        Box::pin(async move {
            let authority = request.uri().authority().map(|a| a.as_str()).unwrap_or("");
            let headers = &request.headers();
            let route_input = RouteInput { authority, headers };
            let route_decision = xds_manager.route(&route_input).await;
            request.extensions_mut().insert(route_decision);
            inner_service.call(request).await
        })
    }
}

/// Tower layer for routing requests to the appropriate cluster based on the `RouteConfiguration`.
#[derive(Clone)]
pub(crate) struct XdsRoutingLayer<E, C> {
    xds_manager: Arc<dyn XdsManager<E, C>>,
}

impl<E, C> XdsRoutingLayer<E, C> {
    /// Creates a new XdsRoutingLayer with the given XdsManager.
    pub(crate) fn new(xds_manager: Arc<dyn XdsManager<E, C>>) -> Self {
        Self { xds_manager }
    }
}

impl<S, E, C> Layer<S> for XdsRoutingLayer<E, C> {
    type Service = XdsRoutingService<S, E, C>;

    fn layer(&self, service: S) -> Self::Service {
        XdsRoutingService {
            inner: service,
            xds_manager: self.xds_manager.clone(),
        }
    }
}