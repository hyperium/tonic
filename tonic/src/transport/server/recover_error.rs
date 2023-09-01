use crate::Status;
use http::Response;
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
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
{
    type Response = Response<MaybeEmptyBody<ResBody>>;
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
{
    type Output = Result<Response<MaybeEmptyBody<ResBody>>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let result: Result<Response<_>, crate::Error> =
            ready!(self.project().inner.poll(cx)).map_err(Into::into);

        match result {
            Ok(response) => {
                let response = response.map(MaybeEmptyBody::full);
                Poll::Ready(Ok(response))
            }
            Err(err) => match Status::try_from_error(err) {
                Ok(status) => {
                    let mut res = Response::new(MaybeEmptyBody::empty());
                    status.add_header(res.headers_mut()).unwrap();
                    Poll::Ready(Ok(res))
                }
                Err(err) => Poll::Ready(Err(err)),
            },
        }
    }
}

#[pin_project]
pub(crate) struct MaybeEmptyBody<B> {
    #[pin]
    inner: Option<B>,
}

impl<B> MaybeEmptyBody<B> {
    fn full(inner: B) -> Self {
        Self { inner: Some(inner) }
    }

    fn empty() -> Self {
        Self { inner: None }
    }
}

impl<B> http_body::Body for MaybeEmptyBody<B>
where
    B: http_body::Body + Send,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        match self.project().inner.as_pin_mut() {
            Some(b) => b.poll_data(cx),
            None => Poll::Ready(None),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        match self.project().inner.as_pin_mut() {
            Some(b) => b.poll_trailers(cx),
            None => Poll::Ready(Ok(None)),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Some(b) => b.is_end_stream(),
            None => true,
        }
    }
}
