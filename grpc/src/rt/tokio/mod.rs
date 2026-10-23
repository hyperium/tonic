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
use std::net::IpAddr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use std::time::Instant;

use tokio::net::TcpStream;
use tokio::task::JoinHandle;

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

#[cfg(feature = "dns")]
mod hickory_resolver;

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

    #[allow(clippy::disallowed_methods)] // This is Tokio's runtime implementation.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        Box::pin(tokio::time::sleep(duration))
    }

    #[allow(clippy::disallowed_methods)] // This is Tokio's runtime implementation.
    fn now(&self) -> Instant {
        // Use tokio's clock so that a paused/advanced test clock is respected.
        tokio::time::Instant::now().into_std()
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
        use tokio::net::UnixStream;

        use crate::client::name_resolution::UNIX_NETWORK_TYPE;

        Box::pin(async move {
            let stream = UnixStream::connect(&path)
                .await
                .map_err(|err| err.to_string())?;
            let peer_addr = stream.peer_addr().map_err(|err| err.to_string())?;
            let local_addr = stream.local_addr().map_err(|err| err.to_string())?;

            let stream: Box<dyn super::GrpcEndpoint> = Box::new(StreamEndpoint {
                peer_addr: format!("{peer_addr:?}").into_boxed_str(),
                local_addr: format!("{local_addr:?}").into_boxed_str(),
                network_type: UNIX_NETWORK_TYPE,
                inner: stream,
            });
            Ok(stream)
        })
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
        Box::pin(async move {
            let listener = tokio::net::UnixListener::bind(&path).map_err(|e| e.to_string())?;
            Ok(Box::new(TokioUnixListener { listener }) as Box<dyn super::EndpointListener>)
        })
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
}

// ---------------------------------------------------------------------------
// TokioTcpListener — EndpointListener for TCP
// ---------------------------------------------------------------------------

/// Wraps `tokio::net::TcpListener` as an [`EndpointListener`](super::EndpointListener).
struct TokioTcpListener {
    listener: tokio::net::TcpListener,
}

#[crate::async_trait]
impl super::EndpointListener for TokioTcpListener {
    async fn accept(&self) -> Result<Box<dyn super::GrpcEndpoint>, String> {
        let (stream, _addr) = self.listener.accept().await.map_err(|e| e.to_string())?;
        let io = StreamEndpoint::new_from_tcp(stream)?;
        Ok(Box::new(io))
    }

    fn local_addr(&self) -> Box<dyn crate::rt::address::ListenerAddress> {
        let addr = self.listener.local_addr().expect("TCP listener has addr");
        Box::new(crate::rt::address::TcpAddress(addr))
    }
}

// ---------------------------------------------------------------------------
// TokioUnixListener — EndpointListener for Unix sockets
// ---------------------------------------------------------------------------

#[cfg(unix)]
struct TokioUnixListener {
    listener: tokio::net::UnixListener,
}

#[cfg(unix)]
#[crate::async_trait]
impl super::EndpointListener for TokioUnixListener {
    async fn accept(&self) -> Result<Box<dyn super::GrpcEndpoint>, String> {
        use crate::client::name_resolution::UNIX_NETWORK_TYPE;

        let (stream, _addr) = self.listener.accept().await.map_err(|e| e.to_string())?;
        let peer_addr = stream.peer_addr().map_err(|e| e.to_string())?;
        let local_addr = stream.local_addr().map_err(|e| e.to_string())?;

        let io: Box<dyn super::GrpcEndpoint> = Box::new(StreamEndpoint {
            peer_addr: format!("{peer_addr:?}").into_boxed_str(),
            local_addr: format!("{local_addr:?}").into_boxed_str(),
            network_type: UNIX_NETWORK_TYPE,
            inner: stream,
        });
        Ok(io)
    }

    fn local_addr(&self) -> Box<dyn crate::rt::address::ListenerAddress> {
        let path = self
            .listener
            .local_addr()
            .map(|a| format!("{a:?}"))
            .unwrap_or_default();
        Box::new(crate::rt::address::UnixListenerAddress::new(path))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::DnsResolver;
    use super::ResolverOptions;
    use super::Runtime;
    use super::TokioDefaultDnsResolver;
    use super::TokioRuntime;

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

    #[tokio::test(start_paused = true)]
    async fn now_reflects_paused_clock() {
        let runtime = TokioRuntime::default();
        let start = runtime.now();

        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(runtime.now() - start, Duration::from_secs(3600));

        runtime.sleep(Duration::from_secs(60)).await;
        assert_eq!(runtime.now() - start, Duration::from_secs(3660));
    }
}
