use super::channel::Channel;
#[cfg(feature = "tls")]
use super::{service::TlsConnector, tls::Certificate};
use bytes::Bytes;
use http::uri::{InvalidUriBytes, Uri};
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
}

impl Endpoint {
    // TODO: determine if we want to expose this or not. This is really
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
    pub fn from_shared(s: impl Into<Bytes>) -> Result<Self, InvalidUriBytes> {
        let uri = Uri::from_shared(s.into())?;
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
    pub fn timeout(&mut self, dur: Duration) -> &mut Self {
        self.timeout = Some(dur);
        self
    }

    /// Apply a concurrency limit to each request.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// builder.concurrency_limit(256);
    /// ```
    pub fn concurrency_limit(&mut self, limit: usize) -> &mut Self {
        self.concurrency_limit = Some(limit);
        self
    }

    /// Apply a rate limit to each request.
    ///
    /// ```
    /// # use tonic::transport::Endpoint;
    /// # use std::time::Duration;
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// builder.rate_limit(32, Duration::from_secs(1));
    /// ```
    pub fn rate_limit(&mut self, limit: u64, duration: Duration) -> &mut Self {
        self.rate_limit = Some((limit, duration));
        self
    }

    /// ```no_run
    /// # use tonic::transport::{Certificate, Endpoint};
    /// # fn dothing() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// let ca = std::fs::read_to_string("ca.pem")?;
    ///
    /// let ca = Certificate::from_pem(ca);
    ///
    /// builder.openssl_tls(ca, "example.com".to_string());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "openssl")]
    pub fn openssl_tls(&mut self, ca: Certificate, domain: impl Into<Option<String>>) -> &mut Self {
        let domain = domain
            .into()
            .unwrap_or_else(|| self.uri.clone().to_string());
        let tls = TlsConnector::new_with_openssl(ca, domain).unwrap();
        self.tls = Some(tls);
        self
    }

    /// ```no_run
    /// # use tonic::transport::{Certificate, Endpoint};
    /// # fn dothing() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut builder = Endpoint::from_static("https://example.com");
    /// let ca = std::fs::read_to_string("ca.pem")?;
    ///
    /// let ca = Certificate::from_pem(ca);
    ///
    /// builder.rustls_tls(ca, "example.com".to_string());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "rustls")]
    pub fn rustls_tls(&mut self, ca: Certificate, domain: impl Into<Option<String>>) -> &mut Self {
        let domain = domain
            .into()
            .unwrap_or_else(|| self.uri.clone().to_string());
        let tls = TlsConnector::new_with_rustls(ca, domain).unwrap();
        self.tls = Some(tls);
        self
    }

    /// Intercept outbound HTTP Request headers;
    pub fn intercept_headers<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn(&mut http::HeaderMap) + Send + Sync + 'static,
    {
        self.interceptor_headers = Some(Arc::new(f));
        self
    }

    /// Create a channel from this config.
    pub fn channel(&self) -> Channel {
        Channel::connect(self.clone())
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
        }
    }
}

impl TryFrom<Bytes> for Endpoint {
    type Error = InvalidUriBytes;

    fn try_from(t: Bytes) -> Result<Self, Self::Error> {
        Self::from_shared(t)
    }
}

impl TryFrom<String> for Endpoint {
    type Error = InvalidUriBytes;

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
