use super::channel::Channel;
#[cfg(feature = "tls")]
use super::{
    service::TlsConnector,
    tls::{Certificate, Identity},
};
use bytes::Bytes;
use http::uri::{InvalidUri, Uri};
use hyper::client::connect::HttpConnector;
use std::{
    convert::{TryFrom, TryInto},
    fmt,
    sync::Arc,
    time::Duration,
};

/// Channel builder.
///
/// This struct is used to build and configure HTTP/2 channels.
#[derive(Clone)]
pub struct Endpoint<C = HttpConnector> {
    pub(super) uri: Uri,
    pub(super) connector: C,
    pub(super) timeout: Option<Duration>,
    pub(super) concurrency_limit: Option<usize>,
    pub(super) rate_limit: Option<(u64, Duration)>,
    pub(super) buffer_size: Option<usize>,
    pub(super) interceptor_headers:
        Option<Arc<dyn Fn(&mut http::HeaderMap) + Send + Sync + 'static>>,
    pub(super) init_stream_window_size: Option<u32>,
    pub(super) init_connection_window_size: Option<u32>,
    pub(super) tcp_keepalive: Option<Duration>,
    pub(super) tcp_nodelay: bool,
}

impl<C> Endpoint<C> {
    // FIXME: determine if we want to expose this or not. This is really
    // just used in codegen for a shortcut.
    #[doc(hidden)]
    pub fn new<D>(dst: D) -> Result<Self, super::Error>
    where
        D: TryInto<Self>,
        D::Error: Into<crate::Error>,
    {
        let me = dst
            .try_into()
            .map_err(|e| super::Error::from_source(super::ErrorKind::Client, e.into()))?;
        Ok(me)
    }

    /// Apply a timeout to each request.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// # use std::time::Duration;
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// builder.timeout(Duration::from_secs(5));
    /// ```
    pub fn timeout(self, dur: Duration) -> Self {
        Endpoint {
            timeout: Some(dur),
            ..self
        }
    }

    /// Set whether TCP keepalive messages are enabled on accepted connections.
    ///
    /// If `None` is specified, keepalive is disabled, otherwise the duration
    /// specified will be the time to remain idle before sending TCP keepalive
    /// probes.
    ///
    /// Default is no keepalive (`None`)
    ///
    pub fn tcp_keepalive(self, tcp_keepalive: Option<Duration>) -> Self {
        Endpoint {
            tcp_keepalive,
            ..self
        }
    }

    /// Apply a concurrency limit to each request.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// builder.concurrency_limit(256);
    /// ```
    pub fn concurrency_limit(self, limit: usize) -> Self {
        Endpoint {
            concurrency_limit: Some(limit),
            ..self
        }
    }

    /// Apply a rate limit to each request.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// # use std::time::Duration;
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// builder.rate_limit(32, Duration::from_secs(1));
    /// ```
    pub fn rate_limit(self, limit: u64, duration: Duration) -> Self {
        Endpoint {
            rate_limit: Some((limit, duration)),
            ..self
        }
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// Default is 65,535
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_INITIAL_WINDOW_SIZE
    pub fn initial_stream_window_size(self, sz: impl Into<Option<u32>>) -> Self {
        Endpoint {
            init_stream_window_size: sz.into(),
            ..self
        }
    }

    /// Sets the max connection-level flow control for HTTP2
    ///
    /// Default is 65,535
    pub fn initial_connection_window_size(self, sz: impl Into<Option<u32>>) -> Self {
        Endpoint {
            init_connection_window_size: sz.into(),
            ..self
        }
    }

    /// Intercept outbound HTTP Request headers;
    pub fn intercept_headers<F>(self, f: F) -> Self
    where
        F: Fn(&mut http::HeaderMap) + Send + Sync + 'static,
    {
        Endpoint {
            interceptor_headers: Some(Arc::new(f)),
            ..self
        }
    }

    /// Configures TLS for the endpoint.
    ///
    /// Shortcut for configuring a TLS connector and calling [`Endpoint::connector`].
    #[cfg(feature = "tls")]
    pub fn tls_config(
        self,
        tls_config: ClientTlsConfig,
    ) -> Endpoint<
        impl tower_make::MakeConnection<
                hyper::Uri,
                Connection = impl Unpin + Send + 'static,
                Future = impl Send + 'static,
                Error = impl Into<Box<dyn std::error::Error + Send + Sync>> + Send,
            > + Clone,
    > {
        let tls_connector = tls_config.tls_connector(self.uri.clone()).unwrap();
        let connector = super::service::tls_connector(Some(tls_connector));
        self.connector(connector)
    }
}

impl<C> Endpoint<C> {
    /// Use a custom connector for the underlying channel.
    ///
    /// Calling [`Endpoint::connect`] requires the connector implement
    /// the [`tower_make::MakeConnection`] requirement, which is an alias for `tower::Service<Uri, Response = AsyncRead +
    /// Async Write>` - for example, a TCP stream for the default [`HttpConnector`].
    ///
    /// # Example
    /// ```rust
    /// use hyper::client::connect::HttpConnector;
    /// use tonic::transport::Endpoint;
    ///
    /// // note: This connector is the same as the default provided in `connect()`.
    /// let mut connector = HttpConnector::new();
    /// connector.enforce_http(false);
    /// connector.set_nodelay(true);
    ///
    /// let endpoint = Endpoint::from_static("http://example.com");
    /// endpoint.connector(connector).connect(); //.await
    /// ```
    ///
    /// # Example with non-default Connector
    /// ```rust
    /// // Use for unix-domain sockets
    /// use hyper_unix_connector::UnixClient;
    /// use tonic::transport::Endpoint;
    ///
    /// let endpoint = Endpoint::from_static("http://example.com");
    /// endpoint.connector(UnixClient).connect(); //.await
    /// ```
    pub fn connector<D>(self, connector: D) -> Endpoint<D> {
        Endpoint {
            uri: self.uri,
            connector,
            concurrency_limit: self.concurrency_limit,
            rate_limit: self.rate_limit,
            timeout: self.timeout,
            buffer_size: self.buffer_size,
            interceptor_headers: self.interceptor_headers,
            init_stream_window_size: self.init_stream_window_size,
            init_connection_window_size: self.init_connection_window_size,
        }
    }
}

impl<C> Endpoint<C>
where
    C: tower_make::MakeConnection<hyper::Uri> + Send + Clone + 'static,
    C::Connection: Unpin + Send + 'static,
    C::Future: Send + 'static,
    C::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send,
{
    /// Create the channel.
    /// Set the value of `TCP_NODELAY` option for accepted connections. Enabled by default.
    pub fn tcp_nodelay(self, enabled: bool) -> Self {
        Endpoint {
            tcp_nodelay: enabled,
            ..self
        }
    }

    /// Create a channel from this config.
    pub async fn connect(&self) -> Result<Channel, super::Error> {
        let e: Self = self.clone();
        Channel::connect(e).await
    }
}

impl Endpoint<HttpConnector> {
    /// Convert an `Endpoint` from a static string.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// Endpoint::from_static("https://example.com");
    /// ```
    pub fn from_static(s: &'static str) -> Self {
        let uri = Uri::from_static(s);
        Self::from(uri)
    }

    /// Convert an `Endpoint` from shared bytes.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// Endpoint::from_shared("https://example.com".to_string());
    /// ```
    pub fn from_shared(s: impl Into<Bytes>) -> Result<Self, InvalidUriBytes> {
        let uri = Uri::from_shared(s.into())?;
        Ok(Self::from(uri))
    }
}

impl From<Uri> for Endpoint<HttpConnector> {
    fn from(uri: Uri) -> Self {
        Self {
            uri,
            connector: super::service::connector(),
            concurrency_limit: None,
            rate_limit: None,
            timeout: None,
            buffer_size: None,
            interceptor_headers: None,
            init_stream_window_size: None,
            init_connection_window_size: None,
            tcp_keepalive: None,
            tcp_nodelay: true,
        }
    }
}

impl TryFrom<Bytes> for Endpoint<HttpConnector> {
    type Error = InvalidUriBytes;

    fn try_from(t: Bytes) -> Result<Self, Self::Error> {
        Self::from_shared(t)
    }
}

impl TryFrom<String> for Endpoint<HttpConnector> {
    type Error = InvalidUriBytes;

    fn try_from(t: String) -> Result<Self, Self::Error> {
        Self::from_shared(t.into_bytes())
    }
}

impl TryFrom<&'static str> for Endpoint<HttpConnector> {
    type Error = Never;

    fn try_from(t: &'static str) -> Result<Self, Self::Error> {
        Ok(Self::from_static(t))
    }
}

#[derive(Debug)]
pub enum Never {}

impl std::fmt::Display for Never {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

impl std::error::Error for Never {}

impl<C> fmt::Debug for Endpoint<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint").finish()
    }
}

/// Configures TLS settings for endpoints.
#[cfg(feature = "tls")]
#[derive(Clone)]
pub struct ClientTlsConfig {
    domain: Option<String>,
    cert: Option<Certificate>,
    identity: Option<Identity>,
    rustls_raw: Option<tokio_rustls::rustls::ClientConfig>,
}

#[cfg(feature = "tls")]
impl fmt::Debug for ClientTlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientTlsConfig")
            .field("domain", &self.domain)
            .field("cert", &self.cert)
            .field("identity", &self.identity)
            .finish()
    }
}

#[cfg(feature = "tls")]
impl ClientTlsConfig {
    /// Creates a new `ClientTlsConfig` using Rustls.
    pub fn with_rustls() -> Self {
        ClientTlsConfig {
            domain: None,
            cert: None,
            identity: None,
            rustls_raw: None,
        }
    }

    /// Sets the domain name against which to verify the server's TLS certificate.
    ///
    /// This has no effect if `rustls_client_config` is used to configure Rustls.
    pub fn domain_name(self, domain_name: impl Into<String>) -> Self {
        ClientTlsConfig {
            domain: Some(domain_name.into()),
            ..self
        }
    }

    /// Sets the CA Certificate against which to verify the server's TLS certificate.
    ///
    /// This has no effect if `rustls_client_config` is used to configure Rustls.
    pub fn ca_certificate(self, ca_certificate: Certificate) -> Self {
        ClientTlsConfig {
            cert: Some(ca_certificate),
            ..self
        }
    }

    /// Sets the client identity to present to the server.
    ///
    /// This has no effect if `rustls_client_config` is used to configure Rustls.
    pub fn identity(self, identity: Identity) -> Self {
        ClientTlsConfig {
            identity: Some(identity),
            ..self
        }
    }

    /// Use options specified by the given `ClientConfig` to configure TLS.
    ///
    /// This overrides all other TLS options set via other means.
    pub fn rustls_client_config(self, config: tokio_rustls::rustls::ClientConfig) -> Self {
        ClientTlsConfig {
            rustls_raw: Some(config),
            ..self
        }
    }

    fn tls_connector(&self, uri: Uri) -> Result<TlsConnector, crate::Error> {
        let domain = match &self.domain {
            None => uri.to_string(),
            Some(domain) => domain.clone(),
        };
        match &self.rustls_raw {
            None => {
                TlsConnector::new_with_rustls_cert(self.cert.clone(), self.identity.clone(), domain)
            }
            Some(c) => TlsConnector::new_with_rustls_raw(c.clone(), domain),
        }
    }
}
