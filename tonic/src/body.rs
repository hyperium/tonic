//! HTTP specific body utilities.
//!
//! This module contains traits and helper types to work with http bodies. Most
//! of the types in this module are based around [`http_body::Body`].

use crate::{Error, Status};
use bytes::{Buf, Bytes};
use http_body::Body as HttpBody;
use std::{
    fmt,
    pin::Pin,
    task::{Context, Poll},
};

/// A trait alias for [`http_body::Body`].
pub trait Body: sealed::Sealed + Send + Sync {
    /// The body data type.
    type Data: Buf;
    /// The errors produced from the body.
    type Error: Into<Error>;

    /// Check if the stream is over or not.
    ///
    /// Reference [`http_body::Body::is_end_stream`].
    fn is_end_stream(&self) -> bool;

    /// Poll for more data from the body.
    ///
    /// Reference [`http_body::Body::poll_data`].
    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>>;

    /// Poll for the trailing headers.
    ///
    /// Reference [`http_body::Body::poll_trailers`].
    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>>;
}

impl<T> Body for T
where
    T: HttpBody + Send + Sync + 'static,
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

/// A type erased http body.
pub struct BoxBody {
    inner: Pin<Box<dyn Body<Data = Bytes, Error = Status> + Send + Sync + 'static>>,
}

struct MapBody<B>(B);

impl BoxBody {
    /// Create a new `BoxBody` mapping item and error to the default types.
    pub fn new<B>(inner: B) -> Self
    where
        B: Body<Data = Bytes, Error = Status> + Send + Sync + 'static,
    {
        BoxBody {
            inner: Box::pin(inner),
        }
    }

    /// Create a new `BoxBody` mapping item and error to the default types.
    pub fn map_from<B>(inner: B) -> Self
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error>,
    {
        BoxBody {
            inner: Box::pin(MapBody(inner)),
        }
    }

    /// Create a new `BoxBody` that is empty.
    pub fn empty() -> Self {
        BoxBody {
            inner: Box::pin(EmptyBody::default()),
        }
    }
}

impl HttpBody for BoxBody {
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        Body::poll_data(self.inner.as_mut(), cx)
    }

    fn poll_trailers(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        Body::poll_trailers(self.inner.as_mut(), cx)
    }
}

impl<B> HttpBody for MapBody<B>
where
    B: Body,
    B::Error: Into<crate::Error>,
{
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let v = unsafe {
            let me = self.get_unchecked_mut();
            Pin::new_unchecked(&mut me.0).poll_data(cx)
        };
        match futures_util::ready!(v) {
            Some(Ok(mut i)) => Poll::Ready(Some(Ok(i.to_bytes()))),
            Some(Err(e)) => {
                let err = Status::map_error(e.into());
                Poll::Ready(Some(Err(err)))
            }
            None => Poll::Ready(None),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        let v = unsafe {
            let me = self.get_unchecked_mut();
            Pin::new_unchecked(&mut me.0).poll_trailers(cx)
        };

        let v = futures_util::ready!(v).map_err(|e| Status::from_error(&*e.into()));
        Poll::Ready(v)
    }
}

impl fmt::Debug for BoxBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoxBody").finish()
    }
}

#[derive(Debug, Default)]
struct EmptyBody {
    _p: (),
}

impl HttpBody for EmptyBody {
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        true
    }

    fn poll_data(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        Poll::Ready(None)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        Poll::Ready(Ok(None))
    }
}
