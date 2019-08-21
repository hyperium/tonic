use crate::{Error, Status};
use bytes::{Buf, Bytes, IntoBuf};
use futures_core::Stream;
use futures_util::{ready, TryStreamExt};
use http::HeaderMap;
use http_body::Body as HttpBody;
use pin_project::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

pub type BytesBuf = <Bytes as IntoBuf>::Buf;

pub trait Body: sealed::Sealed {
    type Data: Buf;
    type Error: Into<Error>;

    fn is_end_stream(&self) -> bool;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>>;

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>>;
}

impl<T> Body for T
where
    T: HttpBody,
    T::Error: Into<Error>,
{
    type Data = T::Data;
    type Error = T::Error;

    fn is_end_stream(&self) -> bool {
        HttpBody::is_end_stream(self)
    }

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        HttpBody::poll_data(self, cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        HttpBody::poll_trailers(self, cx)
    }
}

impl<T> sealed::Sealed for T
where
    T: HttpBody,
    T::Error: Into<Error>,
{
}

mod sealed {
    pub trait Sealed {}
}

pub struct BoxBody {
    inner: Pin<Box<dyn HttpBody<Data = BytesBuf, Error = Status> + Send + 'static>>,
}

impl BoxBody {
    pub fn from_stream<S>(s: S) -> Self
    where
        S: Stream<Item = Result<crate::body::BytesBuf, Status>> + Send + 'static,
    {
        let body = AsyncBody::new(s);
        Self::map_from(body)
    }

    /// Create a new `BoxBody` mapping item and error to the default types.
    pub fn map_from<B>(inner: B) -> Self
    where
        B: HttpBody<Data = BytesBuf, Error = Status> + Send + 'static,
    {
        BoxBody {
            inner: Box::pin(inner),
        }
    }
}

impl HttpBody for BoxBody {
    type Data = BytesBuf;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        HttpBody::is_end_stream(&self.inner)
    }

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        HttpBody::poll_data(self.inner.as_mut(), cx)
    }

    fn poll_trailers(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        HttpBody::poll_trailers(self.inner.as_mut(), cx)
    }
}

#[pin_project]
#[derive(Debug)]
pub struct AsyncBody<S> {
    #[pin]
    inner: S,
    error: Option<Status>,
}

impl<S> AsyncBody<S>
where
    S: Stream<Item = Result<crate::body::BytesBuf, Status>>,
{
    pub fn new(inner: S) -> Self {
        Self { inner, error: None }
    }
}

impl<S> HttpBody for AsyncBody<S>
where
    S: Stream<Item = Result<crate::body::BytesBuf, Status>>,
{
    type Data = BytesBuf;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        false
    }

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let mut self_proj = self.project();
        match ready!(self_proj.inner.try_poll_next_unpin(cx)) {
            Some(Ok(d)) => Some(Ok(d)).into(),
            Some(Err(status)) => {
                *self_proj.error = Some(status);
                None.into()
            }
            None => None.into(),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Status>> {
        // let self_proj = self.project();
        // let status = if let Some(status) = self_proj.error.take() {
        //     status
        // } else {
        //     Status::new(Code::Ok, "")
        // };

        // Poll::Ready(Ok(Some(status.to_header_map()?)))
        Poll::Ready(Ok(None))
    }
}
