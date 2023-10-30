use crate::{body::BoxBody, server::NamedService, transport::BoxFuture};
use http::{Request, Response};
use hyper::Body;
use std::{
    convert::Infallible,
    fmt,
    task::{Context, Poll},
};
use tower::{util::BoxCloneService, ServiceExt};
use tower_service::Service;

/// A [`Service`] router.
#[derive(Default, Clone)]
pub struct Routes {
    router: matchit::Router<BoxCloneService<Request<Body>, Response<BoxBody>, crate::Error>>,
}

impl fmt::Debug for Routes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routes").finish()
    }
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
        S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible>
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
        S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        Self::default().add_service(svc)
    }

    /// Add a new service.
    pub fn add_service<S>(mut self, svc: S) -> Self
    where
        S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let svc = svc.map_err(Into::into);
        self.router
            .insert(format!("/{}/*rest", S::NAME), BoxCloneService::new(svc))
            .unwrap_or_else(|e| panic!("failed to configurate routing: {e}"));
        self
    }
}

async fn unimplemented() -> Result<Response<BoxBody>, crate::Error> {
    let response = Response::builder()
        .status(http::StatusCode::OK)
        .header("grpc-status", "12")
        .header("content-type", "application/grpc")
        .body(crate::body::empty_body())
        .unwrap();
    Ok(response)
}

impl Service<Request<Body>> for Routes {
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        match self.router.at_mut(req.uri().path()) {
            Ok(found) => found.value.call(req),
            Err(_) => Box::pin(unimplemented()),
        }
    }
}
