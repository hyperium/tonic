/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Instant;

use hyper::rt::Executor;
use hyper::rt::Timer;
use pin_project_lite::pin_project;

use crate::private::Token;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

/// Adapts a runtime to a hyper compatible executor.
#[derive(Clone)]
pub(crate) struct HyperCompatExec {
    pub(crate) inner: GrpcRuntime,
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
    pub(crate) inner: GrpcRuntime,
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
    /// A wrapper to make any `GrpcEndpoint` compatible with Hyper. It implements
    /// Tokio's async IO traits.
    pub(crate) struct HyperStream {
        #[pin]
        inner: Box<dyn GrpcEndpoint>,
    }
}

impl HyperStream {
    /// Creates a new `HyperStream` from a type implementing `TcpStream`.
    pub fn new(stream: Box<dyn GrpcEndpoint>) -> Self {
        Self { inner: stream }
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
            match self.project().inner.poll_read_private(cx, &mut tbuf, Token) {
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
        self.project().inner.poll_write_private(cx, buf, Token)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_flush_private(cx, Token)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_shutdown_private(cx, Token)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored_private(Token)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        self.project()
            .inner
            .poll_write_vectored_private(cx, bufs, Token)
    }
}
