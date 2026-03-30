/*
 *
 * Copyright 2026 gRPC authors.
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

use std::io::IoSlice;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;
use tokio_rustls::TlsStream as RustlsStream;

use crate::private;
use crate::rt::AsyncIoAdapter;
use crate::rt::GrpcEndpoint;

pub struct TlsStream<T> {
    inner: RustlsStream<AsyncIoAdapter<T>>,
}

impl<T> GrpcEndpoint for TlsStream<T>
where
    T: GrpcEndpoint,
{
    fn get_local_address(&self) -> &str {
        match &self.inner {
            RustlsStream::Client(s) => s.get_ref().0.get_ref().get_local_address(),
            RustlsStream::Server(s) => s.get_ref().0.get_ref().get_local_address(),
        }
    }

    fn get_peer_address(&self) -> &str {
        match &self.inner {
            RustlsStream::Client(s) => s.get_ref().0.get_ref().get_peer_address(),
            RustlsStream::Server(s) => s.get_ref().0.get_ref().get_peer_address(),
        }
    }

    fn get_network_type(&self) -> &'static str {
        match &self.inner {
            RustlsStream::Client(s) => s.get_ref().0.get_ref().get_network_type(),
            RustlsStream::Server(s) => s.get_ref().0.get_ref().get_network_type(),
        }
    }

    fn poll_read_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        _token: private::Internal,
    ) -> Poll<std::io::Result<()>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        pinned.poll_read(cx, buf)
    }

    fn poll_write_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        _token: private::Internal,
    ) -> Poll<Result<usize, std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        pinned.poll_write(cx, buf)
    }

    fn poll_flush_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _token: private::Internal,
    ) -> Poll<Result<(), std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        pinned.poll_flush(cx)
    }

    fn poll_shutdown_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        _token: private::Internal,
    ) -> Poll<Result<(), std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        pinned.poll_shutdown(cx)
    }

    fn poll_write_vectored_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
        _token: private::Internal,
    ) -> Poll<Result<usize, std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        pinned.poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored_private(&self, _token: private::Internal) -> bool {
        self.inner.is_write_vectored()
    }
}

impl<T: GrpcEndpoint> TlsStream<T> {
    pub(crate) fn new(inner: RustlsStream<AsyncIoAdapter<T>>) -> Self {
        Self { inner }
    }

    pub(crate) fn inner(&self) -> &RustlsStream<AsyncIoAdapter<T>> {
        &self.inner
    }
}
