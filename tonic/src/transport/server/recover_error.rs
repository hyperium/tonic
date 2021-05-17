use super::BoxHttpBody;
use crate::Status;
use bytes::Bytes;
use futures_util::ready;
use http::Response;
use http_body::{Body, Empty};
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

/// Middleware that attempts to recover from service errors by turning them into a response built
/// from the `Status`.
#[derive(Debug, Clone)]
pub(crate) struct RecoverError<S> {
    inner: S,
}

impl<S> RecoverError<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, R, ResBody> Service<R> for RecoverError<S>
where
    S: Service<R, Response = Response<ResBody>>,
    S::Error: Into<crate::Error>,
    ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Response = Response<BoxHttpBody>;
    type Error = crate::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: R) -> Self::Future {
        ResponseFuture {
            inner: self.inner.call(req),
        }
    }
}

#[pin_project]
pub(crate) struct ResponseFuture<F> {
    #[pin]
    inner: F,
}

impl<F, E, ResBody> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
    E: Into<crate::Error>,
    ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Output = Result<Response<BoxHttpBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result: Result<Response<_>, crate::Error> =
            ready!(self.project().inner.poll(cx)).map_err(Into::into);

        match result {
            Ok(response) => {
                let response = response.map(|body| body.map_err(Into::into).boxed());
                Poll::Ready(Ok(response))
            }
            Err(err) => {
                if let Some(status) = Status::try_from_error(&*err) {
                    let mut res = Response::new(Empty::new().map_err(|err| match err {}).boxed());
                    status.add_header(res.headers_mut()).unwrap();
                    Poll::Ready(Ok(res))
                } else {
                    Poll::Ready(Err(err))
                }
            }
        }
    }
}
