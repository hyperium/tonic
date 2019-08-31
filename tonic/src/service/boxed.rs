use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_service::Service;

#[derive(Debug, Clone)]
pub struct BoxService<S> {
    inner: S,
}

impl<S> BoxService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, Request> Service<Request> for BoxService<S>
where
    S: Service<Request>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    // type Future = BoxFuture<'static, Result<Response<Self::ResponseBody>, Self::Error>>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let fut = self.inner.call(request);
        Box::pin(fut)
    }
}
