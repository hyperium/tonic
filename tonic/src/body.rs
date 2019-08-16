use crate::{Code, Status};
use bytes::{Bytes, IntoBuf};
use futures_core::{Stream, TryStream};
use futures_util::{ready, TryStreamExt};
use http::HeaderMap;
use http_body::Body;
use std::pin::Pin;
use std::task::{Context, Poll};

pub type BytesBuf = <Bytes as IntoBuf>::Buf;

pub struct BoxBody {
    inner: Box<dyn Body<Data = BytesBuf, Error = Status> + Send>,
}

impl BoxBody {
    /// Create a new `BoxBody` mapping item and error to the default types.
    pub fn map_from<B>(inner: B) -> Self
    where
        B: Body<Data = BytesBuf, Error = Status> + Send + 'static,
    {
        BoxBody {
            inner: Box::new(inner),
        }
    }
}

impl Body for BoxBody {
    type Data = BytesBuf;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        self.inner.poll_data(cx)
    }

    fn poll_trailers(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        self.inner.poll_trailers(cx)
    }
}

pub struct BoxAsyncBody {
    inner: Pin<Box<dyn Stream<Item = Result<BytesBuf, Status>> + Send>>,
    error: Option<Status>,
}

impl BoxAsyncBody {
    // pub fn new<S>(inner: S) -> Self
    // where
    //     S: Stream<Item = Result<crate::body::BytesBuf, Status>> + Send + 'static,
    // {
    //     Self {
    //         inner: Box::pin(inner),
    //         error: None,
    //     }
    // }

    pub fn new_try<S>(inner: S) -> Self
    where
        S: TryStream<Ok = BytesBuf, Error = Status> + Send + 'static,
    {
        Self {
            inner: Box::pin(inner.into_stream()),
            error: None,
        }
    }
}

impl Body for BoxAsyncBody {
    type Data = BytesBuf;
    type Error = Status;

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        match ready!(self.inner.try_poll_next_unpin(cx)) {
            Some(Ok(d)) => Some(Ok(d)).into(),
            Some(Err(status)) => {
                self.error = Some(status);
                None.into()
            }
            None => None.into(),
        }
    }

    fn poll_trailers(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Option<HeaderMap>, Status>> {
        let status = if let Some(status) = self.error.take() {
            status
        } else {
            Status::new(Code::Ok, "")
        };

        Poll::Ready(Ok(Some(status.to_header_map()?)))
    }
}

// TODO: refactor this to accept an !Unpin stream
#[derive(Debug)]
pub struct AsyncBody<S> {
    inner: S,
    error: Option<Status>,
}

impl<S> AsyncBody<S>
where
    S: Stream<Item = Result<crate::body::BytesBuf, Status>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self { inner, error: None }
    }
}

impl<S> Body for AsyncBody<S>
where
    S: Stream<Item = Result<crate::body::BytesBuf, Status>> + Unpin,
{
    type Data = BytesBuf;
    type Error = Status;

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        match ready!(self.inner.try_poll_next_unpin(cx)) {
            Some(Ok(d)) => Some(Ok(d)).into(),
            Some(Err(status)) => {
                self.error = Some(status);
                None.into()
            }
            None => None.into(),
        }
    }

    fn poll_trailers(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Option<HeaderMap>, Status>> {
        let status = if let Some(status) = self.error.take() {
            status
        } else {
            Status::new(Code::Ok, "")
        };

        Poll::Ready(Ok(Some(status.to_header_map()?)))
    }
}
