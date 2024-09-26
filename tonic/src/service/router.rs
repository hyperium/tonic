use crate::{
    body::{boxed, BoxBody},
    metadata::GRPC_CONTENT_TYPE,
    server::NamedService,
    Status,
};
use http::{HeaderValue, Request, Response};
use std::{
    convert::Infallible,
    fmt,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tower::{Service, ServiceExt};

/// A [`Service`] router.
#[derive(Debug, Clone)]
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

impl Default for Routes {
    fn default() -> Self {
        Self {
            router: axum::Router::new().fallback(unimplemented),
        }
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
        Self::default().add_service(svc)
    }

    /// Create a new empty builder.
    pub fn builder() -> RoutesBuilder {
        RoutesBuilder::default()
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
            svc.map_request(|req: Request<axum::body::Body>| req.map(boxed)),
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
    #[deprecated(since = "0.12.2", note = "Use `Routes::into_axum_router` instead.")]
    pub fn into_router(self) -> axum::Router {
        self.into_axum_router()
    }

    /// Convert this `Routes` into an [`axum::Router`].
    pub fn into_axum_router(self) -> axum::Router {
        self.router
    }
}

impl From<axum::Router> for Routes {
    fn from(router: axum::Router) -> Self {
        Self { router }
    }
}

async fn unimplemented() -> impl axum::response::IntoResponse {
    let status = http::StatusCode::OK;
    let headers = [
        (Status::GRPC_STATUS, HeaderValue::from_static("12")),
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

pub struct RoutesFuture(axum::routing::future::RouteFuture<Infallible>);

impl fmt::Debug for RoutesFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RoutesFuture").finish()
    }
}

impl Future for RoutesFuture {
    type Output = Result<Response<BoxBody>, crate::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(Pin::new(&mut self.as_mut().0).poll(cx)) {
            Ok(res) => Ok(res.map(boxed)).into(),
            // NOTE: This pattern is not needed from Rust 1.82.
            // See https://github.com/rust-lang/rust/pull/122792.
            #[allow(unreachable_patterns)]
            Err(err) => match err {},
        }
    }
}
