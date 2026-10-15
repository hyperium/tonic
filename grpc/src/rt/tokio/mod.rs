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
use std::net::IpAddr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::rt::BoxEndpoint;
use crate::rt::BoxFuture;
use crate::rt::BoxedTaskHandle;
use crate::rt::DnsResolver;
use crate::rt::ResolverOptions;
use crate::rt::Runtime;
use crate::rt::Sleep;
use crate::rt::StreamEndpoint;
use crate::rt::TaskHandle;
use crate::rt::TcpOptions;
use crate::rt::address::FailingSocketAddress;
use crate::rt::address::ListenerAddress;
use crate::rt::address::TcpAddress;

#[cfg(feature = "dns")]
mod hickory_resolver;
#[cfg(unix)]
mod unix;

/// A DNS resolver that uses tokio::net::lookup_host for resolution. It only
/// supports host lookups.
struct TokioDefaultDnsResolver {
    _priv: (),
}

#[crate::async_trait]
impl DnsResolver for TokioDefaultDnsResolver {
    async fn lookup_host_name(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        let name_with_port = match name.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, 0).to_string(),
            Err(_) => format!("{name}:0"),
        };
        let ips = tokio::net::lookup_host(name_with_port)
            .await
            .map_err(|err| err.to_string())?
            .map(|socket_addr| socket_addr.ip())
            .collect();
        Ok(ips)
    }

    async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>, String> {
        Err("TXT record lookup unavailable. Enable the optional 'dns' feature to enable service config lookups.".to_string())
    }
}

#[derive(Debug, Default)]
pub(crate) struct TokioRuntime {
    _priv: (),
}

impl TaskHandle for JoinHandle<()> {
    fn abort(&self) {
        self.abort();
    }
}

impl Sleep for tokio::time::Sleep {}

impl Runtime for TokioRuntime {
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> BoxedTaskHandle {
        Box::new(tokio::spawn(task))
    }

    fn get_dns_resolver(&self, opts: ResolverOptions) -> Result<Box<dyn DnsResolver>, String> {
        #[cfg(feature = "dns")]
        {
            Ok(Box::new(hickory_resolver::DnsResolver::new(opts)?))
        }
        #[cfg(not(feature = "dns"))]
        {
            Ok(Box::new(TokioDefaultDnsResolver::new(opts)?))
        }
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn tcp_stream(
        &self,
        target: SocketAddr,
        opts: TcpOptions,
    ) -> BoxFuture<Result<BoxEndpoint, String>> {
        Box::pin(async move {
            let stream = TcpStream::connect(target)
                .await
                .map_err(|err| err.to_string())?;
            stream
                .set_nodelay(opts.enable_nodelay)
                .map_err(|err| err.to_string())?;
            if let Some(duration) = opts.keepalive {
                let sock_ref = socket2::SockRef::from(&stream);
                let mut ka = socket2::TcpKeepalive::new();
                ka = ka.with_time(duration);
                sock_ref
                    .set_tcp_keepalive(&ka)
                    .map_err(|err| err.to_string())?;
            }
            let stream: Box<dyn super::GrpcEndpoint> =
                Box::new(StreamEndpoint::new_from_tcp(stream)?);
            Ok(stream)
        })
    }

    #[cfg(unix)]
    fn unix_stream(
        &self,
        path: std::path::PathBuf,
        _opts: super::UnixSocketOptions,
    ) -> BoxFuture<Result<Box<dyn super::GrpcEndpoint>, String>> {
        Box::pin(unix::connect(path))
    }

    fn tcp_listener(
        &self,
        addr: SocketAddr,
    ) -> BoxFuture<Result<Box<dyn super::EndpointListener>, String>> {
        Box::pin(async move {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Box::new(TokioTcpListener { listener }) as Box<dyn super::EndpointListener>)
        })
    }

    #[cfg(unix)]
    fn unix_listener(
        &self,
        path: std::path::PathBuf,
        _opts: super::UnixSocketOptions,
    ) -> BoxFuture<Result<Box<dyn super::EndpointListener>, String>> {
        Box::pin(async move { unix::bind(path) })
    }

    #[cfg(not(unix))]
    fn unix_listener(
        &self,
        _path: std::path::PathBuf,
        _opts: super::UnixSocketOptions,
    ) -> BoxFuture<Result<Box<dyn super::EndpointListener>, String>> {
        Box::pin(
            async move { Err("Unix listeners are not supported on this platform".to_string()) },
        )
    }
}

impl TokioDefaultDnsResolver {
    pub fn new(opts: ResolverOptions) -> Result<Self, String> {
        if opts.server_addr.is_some() {
            return Err("Custom DNS server are not supported, enable optional feature 'dns' to enable support.".to_string());
        }
        Ok(TokioDefaultDnsResolver { _priv: () })
    }
}

impl StreamEndpoint<TcpStream> {
    pub(crate) fn new_from_tcp(stream: TcpStream) -> Result<Self, String> {
        Ok(StreamEndpoint {
            local_addr: stream
                .local_addr()
                .map_err(|err| err.to_string())?
                .to_string()
                .into_boxed_str(),
            peer_addr: stream
                .peer_addr()
                .map_err(|err| err.to_string())?
                .to_string()
                .into_boxed_str(),
            network_type: TCP_IP_NETWORK_TYPE,
            inner: stream,
        })
    }

    /// Creates an endpoint for a stream returned by `accept`, which also
    /// returned `peer_addr`. Looking the peer address up again could fail if
    /// the client has already reset the connection. If the local address can't
    /// be read, it is left empty.
    fn new_from_accepted_tcp(stream: TcpStream, peer_addr: SocketAddr) -> Self {
        StreamEndpoint {
            local_addr: stream
                .local_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_default()
                .into_boxed_str(),
            peer_addr: peer_addr.to_string().into_boxed_str(),
            network_type: TCP_IP_NETWORK_TYPE,
            inner: stream,
        }
    }
}

/// How [`accept_with_retry`] backs off after accept failures that aren't
/// per-connection.
struct AcceptRetryPolicy {
    /// Delay after the first such failure.
    initial_backoff: Duration,
    /// Upper bound on the delay between retries.
    max_backoff: Duration,
}

/// Policy used by the tokio TCP and Unix listeners.
const TOKIO_ACCEPT_RETRY_POLICY: AcceptRetryPolicy = AcceptRetryPolicy {
    initial_backoff: Duration::from_millis(5),
    max_backoff: Duration::from_secs(1),
};

/// Reports whether `err` affects only one pending connection, leaving the
/// listener usable.
///
/// Uses the same classification as axum:
/// <https://github.com/tokio-rs/axum/blob/42d6fdc80d8933ced5ff4ae858f43a3581106c75/axum/src/serve/listener.rs#L266-L274>.
fn is_connection_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
    )
}

/// Calls `accept` until it succeeds.
///
/// Per-connection errors are retried immediately. Other errors are retried with
/// exponential back-off, as configured by `policy`, without limit.
// TODO: Every accept error is retried, because there's no simple cross-platform way
// to tell retryable errors from permanent ones. Following other Rust servers is our
// best guess for now.
async fn accept_with_retry<T, F, Fut>(policy: &AcceptRetryPolicy, mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut backoff = policy.initial_backoff;
    loop {
        let err = match accept().await {
            Ok(accepted) => return accepted,
            Err(err) => err,
        };
        if is_connection_error(&err) {
            // A per-connection error means the kernel is still handing us
            // connections, so the listener is healthy: reset the back-off.
            backoff = policy.initial_backoff;
            continue;
        }
        sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(policy.max_backoff);
    }
}

/// Wraps `tokio::net::TcpListener` as an [`EndpointListener`](super::EndpointListener).
struct TokioTcpListener {
    listener: tokio::net::TcpListener,
}

#[crate::async_trait]
impl super::EndpointListener for TokioTcpListener {
    async fn accept(&self) -> Result<Box<dyn super::GrpcEndpoint>, String> {
        let (stream, peer_addr) =
            accept_with_retry(&TOKIO_ACCEPT_RETRY_POLICY, || self.listener.accept()).await;
        Ok(Box::new(StreamEndpoint::new_from_accepted_tcp(
            stream, peer_addr,
        )))
    }

    fn local_addr(&self) -> Box<dyn ListenerAddress> {
        // TODO: Should the API return result or a FailingAddress type?
        match self.listener.local_addr().map_err(|e| e.to_string()) {
            Ok(addr) => Box::new(TcpAddress(addr)),
            Err(err) => Box::new(FailingSocketAddress::new("tcp", err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Ready;
    use std::future::ready;
    use std::io;
    use std::time::Duration;

    use tokio::time::Instant;

    use super::AcceptRetryPolicy;
    use super::DnsResolver;
    use super::ResolverOptions;
    use super::Runtime;
    use super::TokioDefaultDnsResolver;
    use super::TokioRuntime;
    use super::accept_with_retry;

    const TEST_POLICY: AcceptRetryPolicy = AcceptRetryPolicy {
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(40),
    };

    /// Per-connection error, retried immediately.
    fn aborted() -> io::Result<u32> {
        Err(io::Error::from(io::ErrorKind::ConnectionAborted))
    }

    /// Any other error, retried with back-off.
    fn other() -> io::Result<u32> {
        Err(io::Error::other("too many open files"))
    }

    /// Runs `accept_with_retry` with `TEST_POLICY` over `script` and returns the
    /// result, the number of `accept` calls, and the virtual time that passed.
    async fn run_script(script: Vec<io::Result<u32>>) -> (u32, usize, Duration) {
        let mut script = VecDeque::from(script);
        let mut calls = 0;
        let start = Instant::now();
        let result = accept_with_retry(&TEST_POLICY, || -> Ready<io::Result<u32>> {
            calls += 1;
            ready(script.pop_front().expect("script exhausted"))
        })
        .await;
        (result, calls, start.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn accept_with_retry_returns_first_success() {
        let (result, calls, elapsed) = run_script(vec![Ok(1)]).await;

        assert_eq!(result, 1);
        assert_eq!(calls, 1);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn accept_with_retry_retries_connection_errors_immediately() {
        let script = vec![
            aborted(),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
            Err(io::Error::from(io::ErrorKind::ConnectionRefused)),
            Ok(1),
        ];

        let (result, calls, elapsed) = run_script(script).await;

        assert_eq!(result, 1);
        assert_eq!(calls, 4);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn accept_with_retry_backs_off_exponentially() {
        let (result, calls, elapsed) = run_script(vec![other(), other(), Ok(1)]).await;

        assert_eq!(result, 1);
        assert_eq!(calls, 3);
        assert_eq!(elapsed, Duration::from_millis(10 + 20));
    }

    #[tokio::test(start_paused = true)]
    async fn accept_with_retry_caps_backoff() {
        let (result, calls, elapsed) =
            run_script(vec![other(), other(), other(), other(), Ok(1)]).await;

        assert_eq!(result, 1);
        assert_eq!(calls, 5);
        assert_eq!(elapsed, Duration::from_millis(10 + 20 + 40 + 40));
    }

    #[tokio::test(start_paused = true)]
    async fn accept_with_retry_connection_error_resets_backoff() {
        let script = vec![other(), other(), aborted(), other(), Ok(1)];

        let (result, calls, elapsed) = run_script(script).await;

        assert_eq!(result, 1);
        assert_eq!(calls, 5);
        assert_eq!(elapsed, Duration::from_millis(10 + 20 + 10));
    }

    #[tokio::test]
    async fn lookup_hostname() {
        let runtime = TokioRuntime::default();

        let dns = runtime
            .get_dns_resolver(ResolverOptions::default())
            .unwrap();
        let ips = dns.lookup_host_name("localhost").await.unwrap();
        assert!(
            !ips.is_empty(),
            "Expect localhost to resolve to more than 1 IPs."
        );
    }

    #[tokio::test]
    async fn default_resolver_txt_fails() {
        let default_resolver = TokioDefaultDnsResolver::new(ResolverOptions::default()).unwrap();

        let txt = default_resolver.lookup_txt("google.com").await;
        assert!(txt.is_err());
    }

    #[tokio::test]
    async fn default_resolver_custom_authority() {
        let opts = ResolverOptions {
            server_addr: Some("8.8.8.8:53".parse().unwrap()),
        };
        let default_resolver = TokioDefaultDnsResolver::new(opts);
        assert!(default_resolver.is_err());
    }
}
