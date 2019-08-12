use crate::{Code, Status};
use bytes::{Bytes, IntoBuf};
use futures_core::Stream;
use futures_util::{ready, TryStreamExt};
use http::HeaderMap;
use http_body::Body;
use std::task::{Context, Poll};

pub type BytesBuf = <Bytes as IntoBuf>::Buf;

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
