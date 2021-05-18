use crate::{body::BoxBody, Status};
use futures_util::ready;
use http::Response;
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

impl<S, R> Service<R> for RecoverError<S>
where
    S: Service<R, Response = Response<BoxBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = Response<BoxBody>;
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

impl<F, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<BoxBody>, E>>,
    E: Into<crate::Error>,
{
    type Output = Result<Response<BoxBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result: Result<Response<BoxBody>, crate::Error> =
            ready!(self.project().inner.poll(cx)).map_err(Into::into);

        match result {
            Ok(res) => Poll::Ready(Ok(res)),
            Err(err) => {
                if let Some(status) = Status::try_from_error(&*err) {
                    let mut res = Response::new(crate::body::empty_body());
                    status.add_header(res.headers_mut()).unwrap();
                    Poll::Ready(Ok(res))
                } else {
                    Poll::Ready(Err(err))
                }
            }
        }
    }
}
