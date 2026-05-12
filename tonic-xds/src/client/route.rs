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
    /// Channel-level authority used as the routing key.
    authority: Arc<str>,
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
        let authority = self.authority.clone();
        let mut inner_service = self.inner.clone();
        Box::pin(async move {
            let headers = &request.headers();
            let route_input = RouteInput {
                authority: &authority,
                headers,
            };
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
    authority: Arc<str>,
}

impl XdsRoutingLayer {
    /// Creates a new `XdsRoutingLayer` with the given [`Router`] and authority.
    ///
    /// `authority` is the routing key matched against `VirtualHost.domains`
    /// in RDS. It should be the endpoint portion of the xDS target.
    #[allow(dead_code)]
    pub(crate) fn new(router: Arc<dyn Router>, authority: Arc<str>) -> Self {
        Self { router, authority }
    }
}

impl<S> Layer<S> for XdsRoutingLayer {
    type Service = XdsRoutingService<S>;

    fn layer(&self, service: S) -> Self::Service {
        XdsRoutingService {
            inner: service,
            router: self.router.clone(),
            authority: self.authority.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tower::ServiceExt;
    use tower::service_fn;

    /// Mock router that records the `authority` it was called with.
    struct CaptureAuthorityRouter {
        captured: Arc<Mutex<Option<String>>>,
    }

    impl Router for CaptureAuthorityRouter {
        fn route(&self, input: &RouteInput<'_>) -> BoxFuture<Result<RouteDecision, RoutingError>> {
            *self.captured.lock().unwrap() = Some(input.authority.to_string());
            Box::pin(async move {
                Ok(RouteDecision {
                    cluster: "test-cluster".to_string(),
                })
            })
        }
    }

    /// Verifies the routing layer always sources `authority` from its layer
    /// config, not from the request URI.
    #[tokio::test]
    async fn uses_layer_authority_regardless_of_request_uri() {
        let captured = Arc::new(Mutex::new(None));
        let router: Arc<dyn Router> = Arc::new(CaptureAuthorityRouter {
            captured: captured.clone(),
        });
        let layer = XdsRoutingLayer::new(router, Arc::from("greeter.svc:50051"));

        let inner =
            service_fn(
                |_req: Request<()>| async move { Ok::<_, BoxError>(http::Response::new(())) },
            );
        let svc = layer.layer(inner);

        // Case 1: request with no authority on the URI (typical tonic-generated
        // client — see `tonic/src/client/grpc.rs::prepare_request`).
        let req = Request::builder()
            .uri("/pkg.Greeter/SayHello")
            .body(())
            .unwrap();
        svc.clone().oneshot(req).await.unwrap();
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("greeter.svc:50051"),
        );

        // Case 2: request with a different authority on the URI — the layer
        // must still use its own configured authority.
        *captured.lock().unwrap() = None;
        let req = Request::builder()
            .uri("http://other.example:443/pkg.Greeter/SayHello")
            .body(())
            .unwrap();
        svc.oneshot(req).await.unwrap();
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("greeter.svc:50051"),
        );
    }
}
