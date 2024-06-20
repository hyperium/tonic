use crate::{
    body::{boxed, BoxBody},
    metadata::GRPC_CONTENT_TYPE,
    server::NamedService,
};
use http::{HeaderName, HeaderValue, Request, Response};
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
#[derive(Debug, Default, Clone)]
pub struct Routes {
    router: axum::Router,
}

#[derive(Debug, Default, Clone)]
/// Allows adding new services to routes by passing a mutable reference to this builder.
pub struct RoutesBuilder {
    routes: Option<Routes>,
}

impl RoutesBuilder {
    /// Add a new service.
    pub fn add_service<S>(&mut self, svc: S) -> &mut Self
    where
        S: Service<Request<BoxBody>, Response = Response<BoxBody>, Error = Infallible>
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
    pub fn routes(self) -> Routes {
        self.routes.unwrap_or_default()
    }
}
impl Routes {
    /// Create a new routes with `svc` already added to it.
    pub fn new<S>(svc: S) -> Self
    where
        S: Service<Request<BoxBody>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let router = axum::Router::new().fallback(unimplemented);
        Self { router }.add_service(svc)
    }

    /// Add a new service.
    pub fn add_service<S>(mut self, svc: S) -> Self
    where
        S: Service<Request<BoxBody>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        self.router = self.router.route_service(
            &format!("/{}/*rest", S::NAME),
            AxumBodyService { service: svc },
        );
        self
    }

    /// This makes axum perform update some internals of the router that improves perf.
    ///
    /// See <https://docs.rs/axum/latest/axum/routing/struct.Router.html#a-note-about-performance>
    pub fn prepare(self) -> Self {
        Self {
            router: self.router.with_state(()),
        }
    }

    /// Convert this `Routes` into an [`axum::Router`].
    pub fn into_router(self) -> axum::Router {
        self.router
    }
}

async fn unimplemented() -> impl axum::response::IntoResponse {
    let status = http::StatusCode::OK;
    let headers = [
        (
            HeaderName::from_static("grpc-status"),
            HeaderValue::from_static("12"),
        ),
        (http::header::CONTENT_TYPE, GRPC_CONTENT_TYPE),
    ];
    (status, headers)
}

impl Service<Request<BoxBody>> for Routes {
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = RoutesFuture;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
        RoutesFuture(self.router.call(req))
    }
}

#[pin_project]
pub struct RoutesFuture(#[pin] axum::routing::future::RouteFuture<Infallible>);

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

#[derive(Clone)]
struct AxumBodyService<S> {
    service: S,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl<S> Service<Request<axum::body::Body>> for AxumBodyService<S>
where
    S: Service<Request<BoxBody>, Response = Response<BoxBody>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<axum::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: Request<axum::body::Body>) -> Self::Future {
        let fut = self.service.call(req.map(boxed));
        Box::pin(async move { fut.await.map(|res| res.map(axum::body::Body::new)) })
    }
}
