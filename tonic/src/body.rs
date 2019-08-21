use crate::{Error, Status};
use bytes::{Buf, Bytes, IntoBuf};
use http_body::Body as HttpBody;
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
