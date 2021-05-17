use crate::Status;
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_layer::Layer;
use tower_service::Service;

pub fn interceptor_fn<F>(f: F) -> InterceptorFn<F>
where
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
{
    InterceptorFn { f }
}

// TODO(david): don't derive Debug
#[derive(Debug, Clone, Copy)]
pub struct InterceptorFn<F> {
    f: F,
}

impl<S, F> Layer<S> for InterceptorFn<F>
where
    F: Clone,
{
    type Service = InterceptedService<S, F>;

    fn layer(&self, service: S) -> Self::Service {
        InterceptedService {
            inner: service,
            f: self.f.clone(),
        }
    }
}

// TODO(david): don't derive Debug
#[derive(Debug, Clone, Copy)]
pub struct InterceptedService<S, F> {
    inner: S,
    f: F,
}

impl<S, F, ReqBody, ResBody> Service<http::Request<ReqBody>> for InterceptedService<S, F>
where
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = http::Response<ResBody>;
    type Error = crate::Error;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let uri = req.uri().clone();
        let req = crate::Request::from_http(req);
        let (metadata, extensions, msg) = req.into_parts();

        match (self.f)(crate::Request::from_parts(metadata, extensions, ())) {
            Ok(req) => {
                let (metadata, extensions, _) = req.into_parts();
                let req = crate::Request::from_parts(metadata, extensions, msg);
                let req = req.into_http(uri);
                ResponseFuture::future(self.inner.call(req))
            }
            Err(status) => ResponseFuture::error(status),
        }
    }
}

#[pin_project]
#[derive(Debug)]
pub struct ResponseFuture<F> {
    #[pin]
    kind: Kind<F>,
}

impl<F> ResponseFuture<F> {
    fn future(future: F) -> Self {
        Self {
            kind: Kind::Future(future),
        }
    }

    fn error(status: Status) -> Self {
        Self {
            kind: Kind::Error(Some(status)),
        }
    }
}

#[pin_project(project = KindProj)]
#[derive(Debug)]
enum Kind<F> {
    Future(#[pin] F),
    Error(Option<Status>),
}

impl<F, E, B> Future for ResponseFuture<F>
where
    F: Future<Output = Result<http::Response<B>, E>>,
    E: Into<crate::Error>,
{
    type Output = Result<http::Response<B>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().kind.project() {
            KindProj::Future(future) => {
                let response = futures_core::ready!(future.poll(cx).map_err(Into::into)?);
                Poll::Ready(Ok(response))
            }
            KindProj::Error(status) => {
                let error = status.take().unwrap().into();
                Poll::Ready(Err(error))
            }
        }
    }
}
