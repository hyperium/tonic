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
use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

pub(crate) mod hyper_wrapper;
#[cfg(feature = "_runtime-tokio")]
pub(crate) mod tokio;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub(crate) type BoxedTaskHandle = Box<dyn TaskHandle>;

/// An abstraction over an asynchronous runtime.
///
/// The `Runtime` trait defines the core functionality required for
/// executing asynchronous tasks, creating DNS resolvers, and performing
/// time-based operations such as sleeping. It provides a uniform interface
/// that can be implemented for various async runtimes, enabling pluggable
/// and testable infrastructure.
pub(super) trait Runtime: Send + Sync + Debug {
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
}

/// A future that resolves after a specified duration.
pub(super) trait Sleep: Send + Sync + Future<Output = ()> {}

pub(super) trait TaskHandle: Send + Sync {
    /// Abort the associated task.
    fn abort(&self);
}

/// A trait for asynchronous DNS resolution.
#[tonic::async_trait]
pub(super) trait DnsResolver: Send + Sync {
    /// Resolve an address
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<std::net::IpAddr>, String>;
    /// Perform a TXT record lookup. If a txt record contains multiple strings,
    /// they are concatenated.
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>, String>;
}

#[derive(Default)]
pub(super) struct ResolverOptions {
    /// The address of the DNS server in "IP:port" format. If None, the
    /// system's default DNS server will be used.
    pub(super) server_addr: Option<std::net::SocketAddr>,
}

#[derive(Default)]
pub(crate) struct TcpOptions {
    pub(crate) enable_nodelay: bool,
    pub(crate) keepalive: Option<Duration>,
}

mod endpoint {
    /// This trait is sealed since we may need to change the read and write
    /// methods to align closely with the gRPC C++ implementations. For example,
    /// the read method may be responsible for allocating the buffer and
    /// returning it to enable in-place decryption. Since the libraries used
    /// for http2 and channel credentials use AsyncRead, designing such an API
    /// today would require adapters which would incur an extra copy, affecting
    /// performance.
    pub(crate) trait Sealed: tokio::io::AsyncRead + tokio::io::AsyncWrite {}
}

/// GrpcEndpoint is a generic stream-oriented network connection.
pub(crate) trait GrpcEndpoint: endpoint::Sealed + Send + Unpin {
    /// Returns the local address that this stream is bound to.
    fn get_local_address(&self) -> &str;

    /// Returns the remote address that this stream is connected to.
    fn get_peer_address(&self) -> &str;
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
}

pub(crate) fn default_runtime() -> Arc<dyn Runtime> {
    #[cfg(feature = "_runtime-tokio")]
    {
        return Arc::new(tokio::TokioRuntime {});
    }
    #[allow(unreachable_code)]
    Arc::new(NoOpRuntime::default())
}
