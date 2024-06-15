use crate::transport::{
    service::TlsConnector,
    tls::{Certificate, Identity},
    Error,
};
use http::Uri;
use std::fmt;

/// Configures TLS settings for endpoints.
#[derive(Clone, Default)]
pub struct ClientTlsConfig {
    domain: Option<String>,
    certs: Vec<Certificate>,
    identity: Option<Identity>,
    assume_http2: bool,
}

impl fmt::Debug for ClientTlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientTlsConfig")
            .field("domain", &self.domain)
            .field("certs", &self.certs)
            .field("identity", &self.identity)
            .finish()
    }
}

impl ClientTlsConfig {
    /// Creates a new `ClientTlsConfig` using Rustls.
    pub fn new() -> Self {
        ClientTlsConfig {
            domain: None,
            certs: Vec::new(),
            identity: None,
            assume_http2: false,
        }
    }

    /// Sets the domain name against which to verify the server's TLS certificate.
    pub fn domain_name(self, domain_name: impl Into<String>) -> Self {
        ClientTlsConfig {
            domain: Some(domain_name.into()),
            ..self
        }
    }

    /// Sets the CA Certificate against which to verify the server's TLS certificate.
    pub fn ca_certificate(self, ca_certificate: Certificate) -> Self {
        let mut certs = self.certs;
        certs.push(ca_certificate);
        ClientTlsConfig { certs, ..self }
    }

    /// Sets the multiple CA Certificates against which to verify the server's TLS certificate.
    pub fn ca_certificates(self, ca_certificates: impl IntoIterator<Item = Certificate>) -> Self {
        let mut certs = self.certs;
        certs.extend(ca_certificates);
        ClientTlsConfig { certs, ..self }
    }

    /// Sets the client identity to present to the server.
    pub fn identity(self, identity: Identity) -> Self {
        ClientTlsConfig {
            identity: Some(identity),
            ..self
        }
    }

    /// If true, the connector should assume that the server supports HTTP/2,
    /// even if it doesn't provide protocol negotiation via ALPN.
    pub fn assume_http2(self, assume_http2: bool) -> Self {
        ClientTlsConfig {
            assume_http2,
            ..self
        }
    }

    pub(crate) fn tls_connector(&self, uri: Uri) -> Result<TlsConnector, crate::Error> {
        let domain = match &self.domain {
            Some(domain) => domain,
            None => uri.host().ok_or_else(Error::new_invalid_uri)?,
        };
        TlsConnector::new(
            self.certs.clone(),
            self.identity.clone(),
            domain,
            self.assume_http2,
        )
    }
}
