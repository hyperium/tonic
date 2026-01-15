use crate::xds::route::RouteInput;
use crate::xds::xds_manager::XdsRouter;
use http::Request;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{BoxError, Layer, Service};

/// Tower Service for routing requests to the appropriate cluster based on the xDS routing configurations.
/// Attaches routing decision as `RoutingDecision` to the request extensions.
/// The `RoutingDecision` will be used by the `XdsLbService` to identify the xDS cluster to which the request should be routed.
#[derive(Clone)]
pub(crate) struct XdsRoutingService<S> {
    /// The inner Tower Service to which the request will be forwarded after routing decision is made.
    inner: S,
    /// The xDS router used to make routing decisions based on the request and the xDS routing configurations.
    xds_router: Arc<dyn XdsRouter>,
}

impl<S, B> Service<Request<B>> for XdsRoutingService<S>
where
    S: Service<Request<B>, Error = BoxError> + Clone + Send + 'static,
    B: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let xds_router = self.xds_router.clone();
        let mut inner_service = self.inner.clone();
        Box::pin(async move {
            let authority = request
                .uri()
                .authority()
                .map_or("", http::uri::Authority::as_str);
            let headers = &request.headers();
            let route_input = RouteInput { authority, headers };
            let route_decision = xds_router.route(&route_input).await;
            request.extensions_mut().insert(route_decision);
            inner_service.call(request).await
        })
    }
}

/// Tower layer for routing requests to the appropriate cluster based on the `RouteConfiguration`.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct XdsRoutingLayer {
    xds_router: Arc<dyn XdsRouter>,
}

impl XdsRoutingLayer {
    /// Creates a new `XdsRoutingLayer` with the given `XdsRouter`.
    #[allow(dead_code)]
    pub(crate) fn new(xds_router: Arc<dyn XdsRouter>) -> Self {
        Self { xds_router }
    }
}

impl<S> Layer<S> for XdsRoutingLayer {
    type Service = XdsRoutingService<S>;

    fn layer(&self, service: S) -> Self::Service {
        XdsRoutingService {
            inner: service,
            xds_router: self.xds_router.clone(),
        }
    }
}
