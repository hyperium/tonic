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
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::rt::{BoxEndpoint, BoxFuture, ScopedBoxFuture, TcpOptions};

use super::{
    endpoint, BoxedTaskHandle, DnsResolver, GrpcEndpoint, ResolverOptions, Runtime, Sleep,
    TaskHandle,
};

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
        opts: super::TcpOptions,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn super::GrpcEndpoint>, String>> + Send>> {
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
            let stream: Box<dyn super::GrpcEndpoint> = Box::new(TokioTcpStream {
                peer_addr: target.to_string().into_boxed_str(),
                local_addr: stream
                    .local_addr()
                    .map_err(|err| err.to_string())?
                    .to_string()
                    .into_boxed_str(),
                inner: stream,
            });
            Ok(stream)
        })
    }

    fn listen_tcp(
        &self,
        addr: SocketAddr,
        _opts: TcpOptions,
    ) -> BoxFuture<Result<Box<dyn super::TcpListener>, String>> {
        Box::pin(async move {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|err| err.to_string())?;
            let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
            let listener = TokioListener {
                inner: listener,
                local_addr,
            };
            Ok(Box::new(listener) as Box<dyn super::TcpListener>)
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

struct TokioTcpStream {
    inner: TcpStream,
    peer_addr: Box<str>,
    local_addr: Box<str>,
}

impl AsyncRead for TokioTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TokioTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl endpoint::Sealed for TokioTcpStream {}

impl super::GrpcEndpoint for TokioTcpStream {
    fn get_local_address(&self) -> &str {
        &self.local_addr
    }

    fn get_peer_address(&self) -> &str {
        &self.peer_addr
    }
}

struct TokioListener {
    inner: tokio::net::TcpListener,
    local_addr: SocketAddr,
}

impl super::TcpListener for TokioListener {
    fn accept(&mut self) -> ScopedBoxFuture<'_, Result<(BoxEndpoint, SocketAddr), String>> {
        Box::pin(async move {
            let (stream, addr) = self.inner.accept().await.map_err(|e| e.to_string())?;
            Ok((
                Box::new(TokioTcpStream {
                    local_addr: stream
                        .local_addr()
                        .map_err(|err| err.to_string())?
                        .to_string()
                        .into_boxed_str(),
                    peer_addr: addr.to_string().into_boxed_str(),
                    inner: stream,
                }) as Box<dyn GrpcEndpoint>,
                addr,
            ))
        })
    }

    fn local_addr(&self) -> &SocketAddr {
        &self.local_addr
    }
}

#[cfg(test)]
mod tests {
    use super::{DnsResolver, ResolverOptions, Runtime, TokioDefaultDnsResolver, TokioRuntime};

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
