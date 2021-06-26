use bytes::Bytes;
use futures::ready;
use http_body::Body;
use pin_project::pin_project;
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering::Relaxed},
        Arc,
    },
    task::{Context, Poll},
};

/// A body that tracks how many bytes passes through it
#[pin_project]
pub struct CountBytesBody<B> {
    #[pin]
    pub inner: B,
    pub counter: Arc<AtomicUsize>,
}

impl<B> Body for CountBytesBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let this = self.project();
        let counter: Arc<AtomicUsize> = this.counter.clone();
        match ready!(this.inner.poll_data(cx)) {
            Some(Ok(chunk)) => {
                counter.fetch_add(chunk.len(), Relaxed);
                Poll::Ready(Some(Ok(chunk)))
            }
            x => Poll::Ready(x),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        self.project().inner.poll_trailers(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
