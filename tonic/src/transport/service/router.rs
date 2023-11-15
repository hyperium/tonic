use crate::{
    body::{boxed, BoxBody},
    server::NamedService,
    transport::server::BoxError,
};
use bytes::Bytes;
use http::{Request, Response};
use http_body::Body;
use pin_project::pin_project;
use std::{
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tower_service::Service;

/// A [`Service`] router.
#[derive(Debug)]
pub struct Routes<ReqBody = hyper::Body> {
    router: axum::Router<(), ReqBody>,
}

impl<ReqBody> Clone for Routes<ReqBody> {
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
        }
    }
}

impl<ReqBody> Default for Routes<ReqBody>
where
    ReqBody: http_body::Body + Send + 'static,
{
    fn default() -> Self {
        Self {
            router: axum::Router::new(),
        }
    }
}

#[derive(Debug, Default, Clone)]
/// Allows adding new services to routes by passing a mutable reference to this builder.
pub struct RoutesBuilder<ReqBody = hyper::Body> {
    routes: Option<Routes<ReqBody>>,
}

impl<ReqBody> RoutesBuilder<ReqBody>
where
    ReqBody: http_body::Body + Send + 'static,
{
    /// Add a new service.
    pub fn add_service<S>(&mut self, svc: S) -> &mut Self
    where
        S: Service<Request<ReqBody>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let routes = self.routes.take().unwrap_or_default();
        self.routes.replace(routes.add_service(svc));
        self
    }

    /// Returns the routes with added services or empty [`Routes`] if no service was added
    pub fn routes(self) -> Routes<ReqBody> {
        self.routes.unwrap_or_default()
    }
}

impl<ReqBody> Routes<ReqBody>
where
    ReqBody: http_body::Body + Send + 'static,
{
    pub(crate) fn new<S, ResBody>(svc: S) -> Self
    where
        S: Service<Request<ReqBody>, Error = Infallible, Response = Response<ResBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        ResBody: http_body::Body<Data = Bytes> + Send + 'static,
        ResBody::Error: Into<BoxError>,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let router = axum::Router::new().fallback(unimplemented);
        Self { router }.add_service(svc)
    }

    /// Add a new service.
    pub fn add_service<S, ResBody>(mut self, svc: S) -> Self
    where
        S: Service<Request<ReqBody>, Error = Infallible, Response = Response<ResBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        ResBody: http_body::Body<Data = Bytes> + Send + 'static,
        ResBody::Error: Into<BoxError>,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        self.router = self
            .router
            .route_service(&format!("/{}/*rest", S::NAME), svc);
        self
    }

    pub(crate) fn prepare(self) -> Self {
        Self {
            // this makes axum perform update some internals of the router that improves perf
            // see https://docs.rs/axum/latest/axum/routing/struct.Router.html#a-note-about-performance
            router: self.router.with_state(()),
        }
    }

    /// Convert this `Routes` into an [`axum::Router`].
    pub fn into_router(self) -> axum::Router<(), ReqBody> {
        self.router
    }
}

async fn unimplemented() -> impl axum::response::IntoResponse {
    let status = http::StatusCode::OK;
    let headers = [("grpc-status", "12"), ("content-type", "application/grpc")];
    (status, headers)
}

impl<B> Service<Request<B>> for Routes<B>
where
    B: http_body::Body + Send + 'static,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = RoutesFuture<B>;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        RoutesFuture(self.router.call(req))
    }
}

#[pin_project]
pub struct RoutesFuture<B>(#[pin] axum::routing::future::RouteFuture<B, Infallible>);

impl<B> fmt::Debug for RoutesFuture<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RoutesFuture").finish()
    }
}

impl<B> Future for RoutesFuture<B>
where
    B: Body,
{
    type Output = Result<Response<BoxBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.project().0.poll(cx)) {
            Ok(res) => Ok(res.map(boxed)).into(),
            Err(err) => match err {},
        }
    }
}
