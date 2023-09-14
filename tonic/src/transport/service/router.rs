use crate::server::NamedService;
use crate::transport::server::executor::{HasBoxCloneService, HasBoxedCloneService};
use crate::transport::{LocalExec, TokioExec};
use crate::util::BoxCloneService;
use http::{Request, Response};
use hyper::Body;
use std::marker::PhantomData;
use std::{
    convert::Infallible,
    fmt,
    task::{Context, Poll},
};
use tower_service::Service;

/// A [`Service`] router.
#[derive(Clone)]
pub struct Routes<Ex = TokioExec>
where
    Ex: HasBoxCloneService,
{
    router: matchit::Router<Ex::BoxCloneService>,
    _marker: PhantomData<Ex>,
}

/// A type alias of [`Routes`] for thread-local usage.
pub type LocalRoutes = Routes<LocalExec>;

impl<Ex> Default for Routes<Ex>
where
    Ex: HasBoxCloneService,
{
    fn default() -> Self {
        Self {
            router: matchit::Router::default(),
            _marker: PhantomData,
        }
    }
}

impl<Ex> Routes<Ex>
where
    Ex: HasBoxCloneService,
{
    /// Create a new routes with `svc` already added to it.
    pub fn new<S>(svc: S) -> Self
    where
        Ex: HasBoxedCloneService<S>,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
    {
        let router = matchit::Router::default();
        let mut res = Self {
            router,
            _marker: PhantomData,
        };
        res.add_service(svc);
        res
    }

    /// Add a new service.
    pub fn add_service<S>(&mut self, svc: S) -> &mut Self
    where
        Ex: HasBoxedCloneService<S>,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
    {
        self.router
            .insert(format!("/{}/*rest", S::NAME), Ex::boxed_clone_service(svc))
            .unwrap();
        self
    }
}

impl<Ex> fmt::Debug for Routes<Ex>
where
    Ex: HasBoxCloneService,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routes").finish()
    }
}

impl<Ex> Service<Request<Body>> for Routes<Ex>
where
    Ex: HasBoxCloneService,
{
    type Response = Response<<Ex::BoxCloneService as BoxCloneService>::BoxBody>;
    type Error = Infallible;
    type Future = <Ex::BoxCloneService as BoxCloneService>::BoxFuture;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        if let Ok(matched) = self.router.at(req.uri().path()) {
            matched.value.clone().call(req)
        } else {
            <Ex::BoxCloneService as BoxCloneService>::empty_response()
        }
    }
}
