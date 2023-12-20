use crate::{
    body::{boxed, BoxBody},
    server::NamedService,
};
use http::{Request, Response};
use hyper::Body;
use pin_project::pin_project;
use std::{
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tower::ServiceExt;
use tower_service::Service;

/// A [`Service`] router.
#[derive(Debug, Clone)]
pub struct Routes {
    router: axum::Router,
}

#[derive(Debug, Clone)]
/// Allows adding new services to routes by passing a mutable reference to this builder.
pub struct RoutesBuilder {
    router: axum::Router,
}

impl Default for RoutesBuilder {
    fn default() -> Self {
        let router = axum::Router::new().fallback(unimplemented);
        Self { router }
    }
}

impl RoutesBuilder {
    /// Add a new service.
    pub fn add_service<S>(self, svc: S) -> Self
    where
        S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let svc = svc.map_response(|res| res.map(axum::body::boxed));
        let router = self
            .router
            .route_service(&format!("/{}/*rest", S::NAME), svc);
        Self { router }
    }

    /// Returns the routes with added services or empty [`Routes`] if no service was added
    pub fn build(self) -> Routes {
        // this makes axum perform update some internals of the router that improves perf
        // see https://docs.rs/axum/latest/axum/routing/struct.Router.html#a-note-about-performance
        let router = self.router.with_state(());
        Routes { router }
    }
}

impl Routes {
    /// Create a new routes with `svc` already added to it.
    pub fn builder() -> RoutesBuilder {
        RoutesBuilder::default()
    }

    /// Convert this `Routes` into an [`axum::Router`].
    pub fn into_router(self) -> axum::Router {
        self.router
    }
}

async fn unimplemented() -> Response<BoxBody> {
    Response::builder()
        .status(http::StatusCode::OK)
        .header("grpc-status", "12")
        .header("content-type", "application/grpc")
        .body(crate::body::empty_body())
        .unwrap()
}

impl Service<Request<Body>> for Routes {
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = RoutesFuture;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        RoutesFuture(self.router.call(req))
    }
}

#[pin_project]
pub struct RoutesFuture(#[pin] axum::routing::future::RouteFuture<Body, Infallible>);

impl fmt::Debug for RoutesFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RoutesFuture").finish()
    }
}

impl Future for RoutesFuture {
    type Output = Result<Response<BoxBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.project().0.poll(cx)) {
            Ok(res) => Ok(res.map(boxed)).into(),
            Err(err) => match err {},
        }
    }
}
