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

use crate::rt::endpoint;
use crate::rt::GrpcEndpoint;

pub struct TlsStream<T> {
    inner: RustlsStream<T>,
}

impl<T> AsyncRead for TlsStream<T>
where
    T: GrpcEndpoint,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        AsyncRead::poll_read(pinned, cx, buf)
    }
}

impl<T> AsyncWrite for TlsStream<T>
where
    T: GrpcEndpoint,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        AsyncWrite::poll_write(pinned, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        AsyncWrite::poll_flush(pinned, cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        AsyncWrite::poll_shutdown(pinned, cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        let pinned = Pin::new(&mut self.get_mut().inner);
        AsyncWrite::poll_write_vectored(pinned, cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

impl<T> endpoint::Sealed for TlsStream<T> where T: GrpcEndpoint {}

impl<T> GrpcEndpoint for TlsStream<T>
where
    T: GrpcEndpoint,
{
    fn get_local_address(&self) -> &str {
        match &self.inner {
            RustlsStream::Client(s) => s.get_ref().0.get_local_address(),
            RustlsStream::Server(s) => s.get_ref().0.get_local_address(),
        }
    }

    fn get_peer_address(&self) -> &str {
        match &self.inner {
            RustlsStream::Client(s) => s.get_ref().0.get_peer_address(),
            RustlsStream::Server(s) => s.get_ref().0.get_peer_address(),
        }
    }
}

impl<T> TlsStream<T> {
    pub fn new(inner: RustlsStream<T>) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &RustlsStream<T> {
        &self.inner
    }
}
