use super::{channel::Channel, tls::Cert};
use bytes::Bytes;
use http::uri::{InvalidUriBytes, Uri};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub(super) uri: Uri,
    pub(super) timeout: Option<Duration>,
    pub(super) concurrency_limit: Option<usize>,
    pub(super) rate_limit: Option<(u64, Duration)>,
    pub(super) cert: Option<Cert>,
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

    pub fn tls_cert(&mut self, ca: Vec<u8>, domain: Option<String>) -> &mut Self {
        self.cert = Some(Cert {
            ca,
            domain: domain.unwrap_or_else(|| self.uri.clone().to_string()),
            key: None,
        });
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
            cert: None,
        }
    }
}
