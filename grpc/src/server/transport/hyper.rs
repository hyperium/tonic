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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::credentials::ServerCredentials;
use crate::private::Internal;
use crate::rt::EndpointListener;
use crate::rt::GrpcRuntime;
use crate::rt::UnixSocketOptions;
use crate::rt::address::ListenerAddress;
use crate::server::Listener;

mod config;
mod connection;
mod service;
mod stream;
mod validation;

#[cfg(test)]
mod test;

pub use config::Http2Config;
pub use connection::HyperTransport;

/// An HTTP/2 server [`Listener`] backed by Hyper.
pub struct HyperListener {
    listener: Box<dyn EndpointListener>,
    creds: Arc<dyn ServerCredentials>,
    config: Http2Config,
}

impl HyperListener {
    /// Wraps an already-bound [`EndpointListener`] with the default
    /// [`Http2Config`].
    fn new(listener: Box<dyn EndpointListener>, creds: Arc<dyn ServerCredentials>) -> Self {
        Self {
            listener,
            creds,
            config: Http2Config::default(),
        }
    }

    /// Creates an HTTP/2 server listener bound to a TCP address.
    ///
    /// The `creds` parameter specifies the server-side credentials (e.g.,
    /// TLS certificates).
    ///
    /// Returns an error if the runtime fails to bind `addr`.
    pub async fn new_tcp_listener(
        addr: SocketAddr,
        creds: Arc<dyn ServerCredentials>,
        runtime: &GrpcRuntime,
    ) -> Result<Self, String> {
        let listener = runtime.tcp_listener(addr).await?;
        Ok(Self::new(listener, creds))
    }

    /// Creates an HTTP/2 server listener bound to a Unix socket path.
    ///
    /// The `creds` parameter specifies the server-side credentials.
    /// With the Tokio runtime on Linux, paths starting with `\0` are
    /// treated as abstract namespace sockets.
    ///
    /// Returns an error if the runtime fails to bind `path`, or does not
    /// support Unix sockets on this platform.
    pub async fn new_unix_listener(
        path: PathBuf,
        creds: Arc<dyn ServerCredentials>,
        opts: UnixSocketOptions,
        runtime: &GrpcRuntime,
    ) -> Result<Self, String> {
        let listener = runtime.unix_listener(path, opts).await?;
        Ok(Self::new(listener, creds))
    }

    /// Applies the given HTTP/2 configuration to this listener.
    ///
    /// All connections accepted from this listener will use the specified
    /// settings.
    pub fn with_config(mut self, config: Http2Config) -> Self {
        self.config = config;
        self
    }
}

impl Listener for HyperListener {
    type Transport = HyperTransport;

    async fn accept(&self, _token: Internal) -> Option<Result<Self::Transport, String>> {
        let accepted =
            self.listener.accept().await.map(|endpoint| {
                HyperTransport::new(endpoint, self.creds.clone(), self.config.clone())
            });
        Some(accepted)
    }

    fn local_addr(&self) -> Box<dyn ListenerAddress> {
        self.listener.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::net::IpAddr;
    use std::net::Ipv4Addr;
    use std::net::SocketAddr;
    use std::net::TcpListener;
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::Http2Config;
    use super::HyperListener;
    use crate::async_trait;
    use crate::credentials::LocalServerCredentials;
    use crate::private::Internal;
    use crate::rt::BoxEndpoint;
    use crate::rt::EndpointListener;
    use crate::rt::TcpOptions;
    use crate::rt::UnixSocketOptions;
    use crate::rt::address::ListenerAddress;
    use crate::rt::address::TcpAddress;
    use crate::rt::default_runtime;
    use crate::server::Listener;

    const FAKE_ACCEPT_ERROR: &str = "fake accept error";
    const FAKE_LOCAL_ADDR: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 50051);

    /// An [`EndpointListener`] whose `accept` always fails and whose
    /// `local_addr` is a fixed TEST-NET-1 address.
    struct FakeEndpointListener;

    #[async_trait]
    impl EndpointListener for FakeEndpointListener {
        async fn accept(&self) -> Result<BoxEndpoint, String> {
            Err(FAKE_ACCEPT_ERROR.to_string())
        }

        fn local_addr(&self) -> Box<dyn ListenerAddress> {
            Box::new(TcpAddress(FAKE_LOCAL_ADDR))
        }
    }

    fn fake_listener() -> HyperListener {
        HyperListener::new(
            Box::new(FakeEndpointListener),
            Arc::new(LocalServerCredentials::new()),
        )
    }

    #[tokio::test]
    async fn new_tcp_listener_accepts_connection() {
        let rt = default_runtime();
        let listener = HyperListener::new_tcp_listener(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(LocalServerCredentials::new()),
            &rt,
        )
        .await
        .unwrap();
        let local_addr = listener.local_addr();
        let addr = (&*local_addr as &dyn Any)
            .downcast_ref::<TcpAddress>()
            .unwrap()
            .0;
        let _client = rt.tcp_stream(addr, TcpOptions::default()).await.unwrap();

        let _transport = listener
            .accept(Internal)
            .await
            .expect("listener closed")
            .expect("accept failed");
    }

    #[tokio::test]
    async fn new_tcp_listener_returns_error_when_address_in_use() {
        let rt = default_runtime();
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = occupied.local_addr().unwrap();

        let result =
            HyperListener::new_tcp_listener(addr, Arc::new(LocalServerCredentials::new()), &rt)
                .await;

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_unix_listener_accepts_connection() {
        let rt = default_runtime();
        let dir = tempdir().unwrap();
        let path = dir.path().join("hyper.sock");
        let listener = HyperListener::new_unix_listener(
            path.clone(),
            Arc::new(LocalServerCredentials::new()),
            UnixSocketOptions::default(),
            &rt,
        )
        .await
        .unwrap();
        let _client = rt
            .unix_stream(path, UnixSocketOptions::default())
            .await
            .unwrap();

        let _transport = listener
            .accept(Internal)
            .await
            .expect("listener closed")
            .expect("accept failed");
    }

    #[tokio::test]
    async fn new_unix_listener_returns_error_when_bind_fails() {
        let rt = default_runtime();
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing").join("hyper.sock");

        let result = HyperListener::new_unix_listener(
            path,
            Arc::new(LocalServerCredentials::new()),
            UnixSocketOptions::default(),
            &rt,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn accept_returns_endpoint_listener_error() {
        let listener = fake_listener();

        let err = listener
            .accept(Internal)
            .await
            .expect("listener closed")
            .err();

        assert_eq!(err.as_deref(), Some(FAKE_ACCEPT_ERROR));
    }

    #[test]
    fn local_addr_returns_endpoint_listener_address() {
        let listener = fake_listener();

        let local_addr = listener.local_addr();

        let addr = (&*local_addr as &dyn Any).downcast_ref::<TcpAddress>();
        assert_eq!(addr, Some(&TcpAddress(FAKE_LOCAL_ADDR)));
    }

    #[test]
    fn with_config_replaces_default_config() {
        let listener = fake_listener().with_config(Http2Config::new().max_recv_message_size(1024));

        assert_eq!(listener.config.get_max_recv_message_size(), Some(1024));
    }
}
