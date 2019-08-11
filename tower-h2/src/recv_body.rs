use bytes::{Buf, Bytes, BytesMut};
use futures_core::Stream;
use futures_util::TryStreamExt;
use http_body::Body;
use std::task::{Context, Poll};

/// Allows a stream to be read from the remote.
#[derive(Debug)]
pub struct RecvBody {
    inner: h2::RecvStream,
}

#[derive(Debug)]
pub struct Data {
    bytes: Bytes,
}

// ===== impl RecvBody =====

impl RecvBody {
    /// Return a new `RecvBody`.
    pub(crate) fn new(inner: h2::RecvStream) -> Self {
        RecvBody { inner }
    }

    /// Returns the stream ID of the received stream, or `None` if this body
    /// does not correspond to a stream.
    pub fn stream_id(&self) -> h2::StreamId {
        self.inner.stream_id()
    }
}

impl Body for RecvBody {
    type Data = Data;
    type Error = h2::Error;

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Self::Data, h2::Error>>> {
        let data = match futures_util::ready!(self.inner.try_poll_next_unpin(cx)) {
            Some(Ok(bytes)) => {
                self.inner
                    .release_capacity()
                    .release_capacity(bytes.len())
                    .expect("flow control error");
                Data { bytes }
            }
            Some(Err(e)) => return Some(Err(e)).into(),
            None => return None.into(),
        };

        Some(Ok(data)).into()
    }

    fn poll_trailers(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, h2::Error>> {
        match futures_util::ready!(self.inner.poll_trailers(cx)) {
            Some(Ok(t)) => Ok(Some(t)).into(),
            Some(Err(e)) => Err(e).into(),
            None => Ok(None).into(),
        }
    }
}

// ===== impl Data =====

impl Buf for Data {
    fn remaining(&self) -> usize {
        self.bytes.len()
    }

    fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }

    fn advance(&mut self, cnt: usize) {
        self.bytes.advance(cnt);
    }
}

impl From<Data> for Bytes {
    fn from(src: Data) -> Self {
        src.bytes
    }
}

impl From<Data> for BytesMut {
    fn from(src: Data) -> Self {
        src.bytes.into()
    }
}
