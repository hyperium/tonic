use super::channel::Channel;
#[cfg(feature = "tls")]
use super::{
    service::TlsConnector,
    tls::{Certificate, Identity},
};
use bytes::Bytes;
use http::uri::{InvalidUri, Uri};
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
pub struct Endpoint {
    pub(super) uri: Uri,
    pub(super) timeout: Option<Duration>,
    pub(super) concurrency_limit: Option<usize>,
    pub(super) rate_limit: Option<(u64, Duration)>,
    #[cfg(feature = "tls")]
    pub(super) tls: Option<TlsConnector>,
    pub(super) buffer_size: Option<usize>,
    pub(super) interceptor_headers:
        Option<Arc<dyn Fn(&mut http::HeaderMap) + Send + Sync + 'static>>,
    pub(super) init_stream_window_size: Option<u32>,
    pub(super) init_connection_window_size: Option<u32>,
}

impl Endpoint {
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
    #[cfg(feature = "tls")]
    pub fn tls_config(self, tls_config: ClientTlsConfig) -> Self {
        Endpoint {
            tls: Some(tls_config.tls_connector(self.uri.clone()).unwrap()),
            ..self
        }
    }

    /// Create a channel from this config.
    pub async fn connect(&self) -> Result<Channel, super::Error> {
        Channel::connect(self.clone()).await
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
            interceptor_headers: None,
            init_stream_window_size: None,
            init_connection_window_size: None,
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
