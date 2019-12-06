use futures_util::{
    future::Either,
    future::{MapErr, TryFutureExt},
};
use std::{
    fmt,
    sync::Arc,
    task::{Context, Poll},
};
use tower_service::Service;

#[derive(Debug)]
pub(crate) struct Routes<A, B, Request> {
    routes: Or<A, B, Request>,
}

impl<A, B, Request> Routes<A, B, Request> {
    pub(crate) fn new(
        predicate: impl Fn(&Request) -> bool + Send + Sync + 'static,
        a: A,
        b: B,
    ) -> Self {
        let routes = Or::new(predicate, a, b);
        Self { routes }
    }
}

impl<A, B, Request> Routes<A, B, Request> {
    pub(crate) fn push<C>(
        self,
        predicate: impl Fn(&Request) -> bool + Send + Sync + 'static,
        route: C,
    ) -> Routes<C, Or<A, B, Request>, Request> {
        let routes = Or::new(predicate, route, self.routes);
        Routes { routes }
    }
}

impl<A, B, Request> Service<Request> for Routes<A, B, Request>
where
    A: Service<Request>,
    A::Future: Send + 'static,
    A::Error: Into<crate::Error>,
    B: Service<Request, Response = A::Response>,
    B::Future: Send + 'static,
    B::Error: Into<crate::Error>,
{
    type Response = A::Response;
    type Error = crate::Error;
    type Future = <Or<A, B, Request> as Service<Request>>::Future;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.routes.call(req)
    }
}

impl<A: Clone, B: Clone, Request> Clone for Routes<A, B, Request> {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
        }
    }
}

#[doc(hidden)]
pub struct Or<A, B, Request> {
    predicate: Arc<dyn Fn(&Request) -> bool + Send + Sync + 'static>,
    a: A,
    b: B,
}

impl<A, B, Request> Or<A, B, Request> {
    pub(crate) fn new<F>(predicate: F, a: A, b: B) -> Self
    where
        F: Fn(&Request) -> bool + Send + Sync + 'static,
    {
        let predicate = Arc::new(predicate);
        Self { predicate, a, b }
    }
}

impl<A, B, Request> Service<Request> for Or<A, B, Request>
where
    A: Service<Request>,
    A::Future: Send + 'static,
    A::Error: Into<crate::Error>,
    B: Service<Request, Response = A::Response>,
    B::Future: Send + 'static,
    B::Error: Into<crate::Error>,
{
    type Response = A::Response;
    type Error = crate::Error;
    type Future = Either<
        MapErr<A::Future, fn(A::Error) -> crate::Error>,
        MapErr<B::Future, fn(B::Error) -> crate::Error>,
    >;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if (self.predicate)(&req) {
            Either::Left(self.a.call(req).map_err(|e| e.into()))
        } else {
            Either::Right(self.b.call(req).map_err(|e| e.into()))
        }
    }
}

impl<A: Clone, B: Clone, Request> Clone for Or<A, B, Request> {
    fn clone(&self) -> Self {
        Self {
            predicate: self.predicate.clone(),
            a: self.a.clone(),
            b: self.b.clone(),
        }
    }
}

impl<A, B, Request> fmt::Debug for Or<A, B, Request> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Or {{ .. }}")
    }
}
