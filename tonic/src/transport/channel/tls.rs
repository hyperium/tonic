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
    cert: Option<Certificate>,
    identity: Option<Identity>,
}

impl fmt::Debug for ClientTlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientTlsConfig")
            .field("domain", &self.domain)
            .field("cert", &self.cert)
            .field("identity", &self.identity)
            .finish()
    }
}

impl ClientTlsConfig {
    /// Creates a new `ClientTlsConfig` using Rustls.
    pub fn new() -> Self {
        ClientTlsConfig {
            domain: None,
            cert: None,
            identity: None,
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
        ClientTlsConfig {
            cert: Some(ca_certificate),
            ..self
        }
    }

    /// Sets the client identity to present to the server.
    pub fn identity(self, identity: Identity) -> Self {
        ClientTlsConfig {
            identity: Some(identity),
            ..self
        }
    }

    pub(crate) fn tls_connector(&self, uri: Uri) -> Result<TlsConnector, crate::Error> {
        let domain = match &self.domain {
            Some(domain) => domain,
            None => uri.host().ok_or_else(Error::new_invalid_uri)?,
        };
        TlsConnector::new(self.cert.clone(), self.identity.clone(), domain)
    }
}
