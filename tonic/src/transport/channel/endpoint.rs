use super::super::service;
use super::Channel;
#[cfg(feature = "tls")]
use super::ClientTlsConfig;
#[cfg(feature = "tls")]
use crate::transport::service::TlsConnector;
use crate::transport::Error;
use bytes::Bytes;
use http::uri::{InvalidUri, Uri};
use std::{
    convert::{TryFrom, TryInto},
    fmt,
    time::Duration,
};
use tower_make::MakeConnection;

/// Channel builder.
///
/// This struct is used to build and configure HTTP/2 channels.
#[derive(Clone)]
pub struct Endpoint {
    pub(crate) uri: Uri,
    pub(crate) timeout: Option<Duration>,
    pub(crate) concurrency_limit: Option<usize>,
    pub(crate) rate_limit: Option<(u64, Duration)>,
    #[cfg(feature = "tls")]
    pub(crate) tls: Option<TlsConnector>,
    pub(crate) buffer_size: Option<usize>,
    pub(crate) init_stream_window_size: Option<u32>,
    pub(crate) init_connection_window_size: Option<u32>,
    pub(crate) tcp_keepalive: Option<Duration>,
    pub(crate) tcp_nodelay: bool,
    pub(crate) http2_keep_alive_interval: Option<Duration>,
    pub(crate) http2_keep_alive_timeout: Option<Duration>,
    pub(crate) http2_keep_alive_while_idle: Option<bool>,
}

impl Endpoint {
    // FIXME: determine if we want to expose this or not. This is really
    // just used in codegen for a shortcut.
    #[doc(hidden)]
    pub fn new<D>(dst: D) -> Result<Self, Error>
    where
        D: TryInto<Self>,
        D::Error: Into<crate::Error>,
    {
        let me = dst.try_into().map_err(|e| Error::from_source(e.into()))?;
        Ok(me)
    }

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
    pub fn from_shared(s: impl Into<Bytes>) -> Result<Self, InvalidUri> {
        let uri = Uri::from_maybe_shared(s.into())?;
        Ok(Self::from(uri))
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

    /// Configures TLS for the endpoint.
    #[cfg(feature = "tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
    pub fn tls_config(self, tls_config: ClientTlsConfig) -> Self {
        Endpoint {
            tls: Some(tls_config.tls_connector(self.uri.clone()).unwrap()),
            ..self
        }
    }

    /// Set the value of `TCP_NODELAY` option for accepted connections. Enabled by default.
    pub fn tcp_nodelay(self, enabled: bool) -> Self {
        Endpoint {
            tcp_nodelay: enabled,
            ..self
        }
    }

    /// Set http2 KEEP_ALIVE_INTERVAL. Uses `hyper`'s default otherwise.
    pub fn http2_keep_alive_interval(self, interval: Duration) -> Self {
        Endpoint {
            http2_keep_alive_interval: Some(interval),
            ..self
        }
    }

    /// Set http2 KEEP_ALIVE_TIMEOUT. Uses `hyper`'s default otherwise.
    pub fn keep_alive_timeout(self, duration: Duration) -> Self {
        Endpoint {
            http2_keep_alive_timeout: Some(duration),
            ..self
        }
    }

    /// Set http2 KEEP_ALIVE_WHILE_IDLE. Uses `hyper`'s default otherwise.
    pub fn keep_alive_while_idle(self, enabled: bool) -> Self {
        Endpoint {
            http2_keep_alive_while_idle: Some(enabled),
            ..self
        }
    }

    /// Create a channel from this config.
    pub async fn connect(&self) -> Result<Channel, Error> {
        let mut http = hyper::client::connect::HttpConnector::new();
        http.enforce_http(false);
        http.set_nodelay(self.tcp_nodelay);
        http.set_keepalive(self.tcp_keepalive);

        #[cfg(feature = "tls")]
        let connector = service::connector(http, self.tls.clone());

        #[cfg(not(feature = "tls"))]
        let connector = service::connector(http);

        Channel::connect(connector, self.clone()).await
    }

    /// Connect with a custom connector.
    pub async fn connect_with_connector<C>(&self, connector: C) -> Result<Channel, Error>
    where
        C: MakeConnection<Uri> + Send + 'static,
        C::Connection: Unpin + Send + 'static,
        C::Future: Send + 'static,
        crate::Error: From<C::Error> + Send + 'static,
    {
        #[cfg(feature = "tls")]
        let connector = service::connector(connector, self.tls.clone());

        #[cfg(not(feature = "tls"))]
        let connector = service::connector(connector);

        Channel::connect(connector, self.clone()).await
    }
}

impl From<Uri> for Endpoint {
    fn from(uri: Uri) -> Self {
        Self {
            uri,
            concurrency_limit: None,
            rate_limit: None,
            timeout: None,
            #[cfg(feature = "tls")]
            tls: None,
            buffer_size: None,
            init_stream_window_size: None,
            init_connection_window_size: None,
            tcp_keepalive: None,
            tcp_nodelay: true,
            http2_keep_alive_interval: None,
            http2_keep_alive_timeout: None,
            http2_keep_alive_while_idle: None,
        }
    }
}

impl TryFrom<Bytes> for Endpoint {
    type Error = InvalidUri;

    fn try_from(t: Bytes) -> Result<Self, Self::Error> {
        Self::from_shared(t)
    }
}

impl TryFrom<String> for Endpoint {
    type Error = InvalidUri;

    fn try_from(t: String) -> Result<Self, Self::Error> {
        Self::from_shared(t.into_bytes())
    }
}

impl TryFrom<&'static str> for Endpoint {
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

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint").finish()
    }
}
