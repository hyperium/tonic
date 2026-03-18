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

use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::io::IoSlice;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use ::tokio::io::AsyncRead;
use ::tokio::io::AsyncWrite;
use ::tokio::io::ReadBuf;

use crate::private::Token;

pub(crate) mod hyper_wrapper;
#[cfg(feature = "_runtime-tokio")]
pub(crate) mod tokio;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type BoxedTaskHandle = Box<dyn TaskHandle>;
pub type BoxEndpoint = Box<dyn GrpcEndpoint>;
pub type ScopedBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An abstraction over an asynchronous runtime.
///
/// The `Runtime` trait defines the core functionality required for
/// executing asynchronous tasks, creating DNS resolvers, and performing
/// time-based operations such as sleeping. It provides a uniform interface
/// that can be implemented for various async runtimes, enabling pluggable
/// and testable infrastructure.
pub trait Runtime: Send + Sync + Debug {
    /// Spawns the given asynchronous task to run in the background.
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> BoxedTaskHandle;

    /// Creates and returns an instance of a DNSResolver, optionally
    /// configured by the ResolverOptions struct. This method may return an
    /// error if it fails to create the DNSResolver.
    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String>;

    /// Returns a future that completes after the specified duration.
    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>>;

    /// Establishes a TCP connection to the given `target` address with the
    /// specified `opts`.
    fn tcp_stream(
        &self,
        target: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn GrpcEndpoint>, String>>;

    /// Create a new listener for the given address.
    fn listen_tcp(
        &self,
        addr: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn TcpListener>, String>>;
}

/// A future that resolves after a specified duration.
pub trait Sleep: Send + Sync + Future<Output = ()> {}

pub trait TaskHandle: Send + Sync {
    /// Abort the associated task.
    fn abort(&self);
}

/// A trait for asynchronous DNS resolution.
#[tonic::async_trait]
pub trait DnsResolver: Send + Sync {
    /// Resolve an address
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<std::net::IpAddr>, String>;
    /// Perform a TXT record lookup. If a txt record contains multiple strings,
    /// they are concatenated.
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String>;
}

#[derive(Default)]
pub struct ResolverOptions {
    /// The address of the DNS server in "IP:port" format. If None, the
    /// system's default DNS server will be used.
    pub(super) server_addr: Option<std::net::SocketAddr>,
}

#[derive(Default)]
pub struct TcpOptions {
    pub(crate) enable_nodelay: bool,
    pub(crate) keepalive: Option<Duration>,
}

/// GrpcEndpoint is a generic stream-oriented network connection.
// This trait is sealed since we may need to change the read and write
// methods to align closely with the gRPC C++ implementations. For example,
// the read method may be responsible for allocating the buffer and
// returning it to enable in-place decryption. Since the libraries used
// for http2 and channel credentials use AsyncRead, designing such an API
// today would require adapters which would incur an extra copy, affecting
// performance.
pub trait GrpcEndpoint: Send + Unpin + 'static {
    /// Returns the local address that this stream is bound to.
    fn get_local_address(&self) -> &str;

    /// Returns the remote address that this stream is connected to.
    fn get_peer_address(&self) -> &str;

    /// Returns the network type of the connection (e.g., "tcp", "unix").
    fn get_network_type(&self) -> &'static str;

    #[doc(hidden)]
    fn poll_read_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        token: Token,
    ) -> Poll<io::Result<()>>;

    #[doc(hidden)]
    fn poll_write_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        token: Token,
    ) -> Poll<io::Result<usize>>;

    #[doc(hidden)]
    fn poll_flush_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        token: Token,
    ) -> Poll<io::Result<()>>;

    #[doc(hidden)]
    fn poll_shutdown_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        token: Token,
    ) -> Poll<io::Result<()>>;

    #[doc(hidden)]
    fn poll_write_vectored_private(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
        token: Token,
    ) -> Poll<io::Result<usize>> {
        let buf = bufs
            .iter()
            .find(|b| !b.is_empty())
            .map_or(&[][..], |b| &**b);
        self.poll_write_private(cx, buf, token)
    }

    #[doc(hidden)]
    fn is_write_vectored_private(&self, _: Token) -> bool {
        false
    }
}

/// An adapter that exposes `AsyncRead` and `AsyncWrite` functionality for
/// interfacing with `hyper` and `rustls`. This type is kept private to avoid
/// exposing its read and write methods to external crates.
pub(crate) struct AsyncIoAdapter<T> {
    inner: T,
}

impl<T: GrpcEndpoint> AsyncIoAdapter<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self { inner }
    }

    pub(crate) fn get_ref(&self) -> &T {
        &self.inner
    }
}

impl<T: GrpcEndpoint> AsyncRead for AsyncIoAdapter<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read_private(cx, buf, Token)
    }
}

impl<T: GrpcEndpoint> AsyncWrite for AsyncIoAdapter<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_private(cx, buf, Token)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush_private(cx, Token)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown_private(cx, Token)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored_private(cx, bufs, Token)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored_private(Token)
    }
}

impl GrpcEndpoint for Box<dyn GrpcEndpoint> {
    fn get_local_address(&self) -> &str {
        (**self).get_local_address()
    }

    fn get_peer_address(&self) -> &str {
        (**self).get_peer_address()
    }

    fn get_network_type(&self) -> &'static str {
        (**self).get_network_type()
    }

    fn poll_read_private(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        token: Token,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut **self).poll_read_private(cx, buf, token)
    }

    fn poll_write_private(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        token: Token,
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut **self).poll_write_private(cx, buf, token)
    }

    fn poll_flush_private(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        token: Token,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut **self).poll_flush_private(cx, token)
    }

    fn poll_shutdown_private(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        token: Token,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut **self).poll_shutdown_private(cx, token)
    }
}

/// A trait representing a TCP listener capable of accepting incoming
/// connections.
pub trait TcpListener: Send + Sync {
    /// Accepts a new incoming connection.
    ///
    /// Returns a future that resolves to a result containing the new
    /// `GrpcEndpoint` and the remote peer's `SocketAddr`, or an error string
    /// if acceptance fails.
    fn accept(&mut self) -> ScopedBoxFuture<'_, Result<(BoxEndpoint, SocketAddr), String>>;

    /// Returns the local socket address this listener is bound to.
    fn local_addr(&self) -> &SocketAddr;
}

/// A fake runtime to satisfy the compiler when no runtime is enabled. This will
///
/// # Panics
///
/// Panics if any of its functions are called.
#[derive(Default, Debug)]
pub(crate) struct NoOpRuntime {}

impl Runtime for NoOpRuntime {
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> BoxedTaskHandle {
        unimplemented!()
    }

    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String> {
        unimplemented!()
    }

    fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>> {
        unimplemented!()
    }

    fn tcp_stream(
        &self,
        target: SocketAddr,
        opts: TcpOptions,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn GrpcEndpoint>, String>> + Send>> {
        unimplemented!()
    }

    fn listen_tcp(
        &self,
        addr: SocketAddr,
        _opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn TcpListener>, String>> {
        unimplemented!()
    }
}

pub(crate) fn default_runtime() -> GrpcRuntime {
    #[cfg(feature = "_runtime-tokio")]
    {
        return GrpcRuntime::new(tokio::TokioRuntime::default());
    }
    #[allow(unreachable_code)]
    GrpcRuntime::new(NoOpRuntime::default())
}

#[derive(Clone, Debug)]
pub struct GrpcRuntime {
    inner: Arc<dyn Runtime>,
}

impl GrpcRuntime {
    pub fn new<T: Runtime + 'static>(runtime: T) -> Self {
        GrpcRuntime {
            inner: Arc::new(runtime),
        }
    }

    pub fn spawn(
        &self,
        task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
    ) -> BoxedTaskHandle {
        self.inner.spawn(task)
    }

    pub fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String> {
        self.inner.get_dns_resolver(opts)
    }

    pub fn sleep(&self, duration: std::time::Duration) -> Pin<Box<dyn Sleep>> {
        self.inner.sleep(duration)
    }

    pub fn tcp_stream(
        &self,
        target: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn GrpcEndpoint>, String>> {
        self.inner.tcp_stream(target, opts)
    }

    pub fn listen_tcp(
        &self,
        addr: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn TcpListener>, String>> {
        self.inner.listen_tcp(addr, opts)
    }
}
