use hyper::rt::{Executor, Timer};
use pin_project_lite::pin_project;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{Runtime, TcpStream};

/// Adapts a runtime to a hyper compatible executor.
#[derive(Clone)]
pub(crate) struct HyperCompatExec {
    pub(crate) inner: Arc<dyn Runtime>,
}

impl<F> Executor<F> for HyperCompatExec
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        self.inner.spawn(Box::pin(async {
            let _ = fut.await;
        }));
    }
}

struct HyperCompatSleep {
    inner: Pin<Box<dyn super::Sleep>>,
}

impl Future for HyperCompatSleep {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

impl hyper::rt::Sleep for HyperCompatSleep {}

/// Adapts a runtime to a hyper compatible timer.
pub(crate) struct HyperCompatTimer {
    pub(crate) inner: Arc<dyn Runtime>,
}

impl Timer for HyperCompatTimer {
    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        let sleep = self.inner.sleep(duration);
        Box::pin(HyperCompatSleep { inner: sleep })
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        let now = Instant::now();
        let duration = deadline.saturating_duration_since(now);
        self.sleep(duration)
    }
}

// The following adapters are copied from hyper:
// https://github.com/hyperium/hyper/blob/v1.6.0/benches/support/tokiort.rs

pin_project! {
    /// A wrapper to make any `TcpStream` compatible with Hyper. It implements
    /// Tokio's async IO traits.
    pub(crate) struct HyperStream {
        #[pin]
        inner: Box<dyn TcpStream>,
    }
}

impl HyperStream {
    /// Creates a new `HyperStream` from a type implementing `TcpStream`.
    pub fn new(stream: Box<dyn TcpStream>) -> Self {
        Self { inner: stream }
    }
}

impl AsyncRead for HyperStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Delegate the poll_read call to the inner stream.
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for HyperStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl hyper::rt::Read for HyperStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let n = unsafe {
            let mut tbuf = tokio::io::ReadBuf::uninit(buf.as_mut());
            match tokio::io::AsyncRead::poll_read(self.project().inner, cx, &mut tbuf) {
                Poll::Ready(Ok(())) => tbuf.filled().len(),
                other => return other,
            }
        };

        unsafe {
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for HyperStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        AsyncWrite::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_shutdown(self.project().inner, cx)
    }

    fn is_write_vectored(&self) -> bool {
        AsyncWrite::is_write_vectored(&self.inner)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        AsyncWrite::poll_write_vectored(self.project().inner, cx, bufs)
    }
}
