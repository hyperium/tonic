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

use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::private;
use crate::rt::BoxEndpoint;
use crate::rt::BoxFuture;
use crate::rt::BoxedTaskHandle;
use crate::rt::DnsResolver;
use crate::rt::GrpcEndpoint;
use crate::rt::ResolverOptions;
use crate::rt::Runtime;
use crate::rt::Sleep;
use crate::rt::TaskHandle;
use crate::rt::TcpOptions;

#[cfg(feature = "dns")]
mod hickory_resolver;

/// A DNS resolver that uses tokio::net::lookup_host for resolution. It only
/// supports host lookups.
struct TokioDefaultDnsResolver {
    _priv: (),
}

#[tonic::async_trait]
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
        self.abort()
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
            if let Some(duration) = opts.keepalive {
                let sock_ref = socket2::SockRef::from(&stream);
                let mut ka = socket2::TcpKeepalive::new();
                ka = ka.with_time(duration);
                sock_ref
                    .set_tcp_keepalive(&ka)
                    .map_err(|err| err.to_string())?;
            }

            let stream: Box<dyn super::GrpcEndpoint> =
                Box::new(TokioIoStream::new_from_tcp(stream)?);
            Ok(stream)
        })
    }

    #[cfg(unix)]
    fn unix_stream(
        &self,
        path: std::path::PathBuf,
        _opts: super::UnixSocketOptions,
    ) -> BoxFuture<Result<Box<dyn GrpcEndpoint>, String>> {
        use tokio::net::UnixStream;

        use crate::client::name_resolution::UNIX_NETWORK_TYPE;

        Box::pin(async move {
            let stream = UnixStream::connect(&path)
                .await
                .map_err(|err| err.to_string())?;
            let peer_addr = stream.peer_addr().map_err(|err| err.to_string())?;
            let local_addr = stream.local_addr().map_err(|err| err.to_string())?;

            let stream: Box<dyn super::GrpcEndpoint> = Box::new(TokioIoStream {
                peer_addr: format!("{peer_addr:?}").into_boxed_str(),
                local_addr: format!("{local_addr:?}").into_boxed_str(),
                network_type: UNIX_NETWORK_TYPE,
                inner: stream,
            });
            Ok(stream)
        })
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

pub(crate) struct TokioIoStream<T> {
    inner: T,
    peer_addr: Box<str>,
    local_addr: Box<str>,
    network_type: &'static str,
}

impl TokioIoStream<TcpStream> {
    pub(crate) fn new_from_tcp(stream: TcpStream) -> Result<Self, String> {
        Ok(TokioIoStream {
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

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> super::GrpcEndpoint for TokioIoStream<T> {
    fn get_local_address(&self) -> &str {
        &self.local_addr
    }

    fn get_peer_address(&self) -> &str {
        &self.peer_addr
    }

    fn get_network_type(&self) -> &'static str {
        self.network_type
    }

    fn poll_read_private(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
        _token: private::Internal,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }

    fn poll_write_private(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
        _token: private::Internal,
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored_private(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
        _token: private::Internal,
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored_private(&self, _token: private::Internal) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush_private(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        _token: private::Internal,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown_private(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        _token: private::Internal,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
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
        )
    }

    #[tokio::test]
    async fn default_resolver_txt_fails() {
        let default_resolver = TokioDefaultDnsResolver::new(ResolverOptions::default()).unwrap();

        let txt = default_resolver.lookup_txt("google.com").await;
        assert!(txt.is_err())
    }

    #[tokio::test]
    async fn default_resolver_custom_authority() {
        let opts = ResolverOptions {
            server_addr: Some("8.8.8.8:53".parse().unwrap()),
        };
        let default_resolver = TokioDefaultDnsResolver::new(opts);
        assert!(default_resolver.is_err())
    }
}
