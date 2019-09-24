use super::channel::Channel;
#[cfg(feature = "tls")]
use super::{service::TlsConnector, tls::Certificate};
use bytes::Bytes;
use http::uri::{InvalidUriBytes, Uri};
use std::{convert::TryFrom, time::Duration};

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub(super) uri: Uri,
    pub(super) timeout: Option<Duration>,
    pub(super) concurrency_limit: Option<usize>,
    pub(super) rate_limit: Option<(u64, Duration)>,
    #[cfg(feature = "tls")]
    pub(super) tls: Option<TlsConnector>,
}

impl Endpoint {
    pub fn from_static(s: &'static str) -> Self {
        let uri = Uri::from_static(s);
        Self::from(uri)
    }

    pub fn from_shared(s: impl Into<Bytes>) -> Result<Self, InvalidUriBytes> {
        let uri = Uri::from_shared(s.into())?;
        Ok(Self::from(uri))
    }

    pub fn timeout(&mut self, dur: Duration) -> &mut Self {
        self.timeout = Some(dur);
        self
    }

    pub fn concurrency_limit(&mut self, limit: usize) -> &mut Self {
        self.concurrency_limit = Some(limit);
        self
    }

    pub fn rate_limit(&mut self, limit: u64, duration: Duration) -> &mut Self {
        self.rate_limit = Some((limit, duration));
        self
    }

    #[cfg(feature = "openssl")]
    pub fn openssl_tls(&mut self, ca: Certificate, domain: Option<String>) -> &mut Self {
        let domain = domain.unwrap_or_else(|| self.uri.clone().to_string());
        let tls = TlsConnector::new_with_openssl(ca, domain).unwrap();
        self.tls = Some(tls);
        self
    }

    #[cfg(feature = "rustls")]
    pub fn rustls_tls(&mut self, ca: Certificate, domain: Option<String>) -> &mut Self {
        let domain = domain.unwrap_or_else(|| self.uri.clone().to_string());
        let tls = TlsConnector::new_with_rustls(ca, domain).unwrap();
        self.tls = Some(tls);
        self
    }

    // pub fn metadata_interceptor(f: impl Fn(MetadataMap) ->)

    pub fn channel(&self) -> Result<Channel, super::Error> {
        Channel::builder().connect(self.clone())
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
