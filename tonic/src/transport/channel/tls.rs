use super::service::TlsConnector;
use crate::transport::{
    Error,
    tls::{Certificate, Identity},
};
use http::Uri;
use std::sync::Arc;
use std::time::Duration;
use tokio_rustls::rustls::client::danger::ServerCertVerifier;
use tokio_rustls::rustls::pki_types::TrustAnchor;

/// Configures TLS settings for endpoints.
#[derive(Debug, Clone, Default)]
pub struct ClientTlsConfig {
    domain: Option<String>,
    certs: Vec<Certificate>,
    trust_anchors: Vec<TrustAnchor<'static>>,
    identity: Option<Identity>,
    assume_http2: bool,
    #[cfg(feature = "tls-native-roots")]
    with_native_roots: bool,
    #[cfg(feature = "tls-webpki-roots")]
    with_webpki_roots: bool,
    use_key_log: bool,
    timeout: Option<Duration>,
    server_cert_verifier: Option<Arc<dyn ServerCertVerifier>>,
}

impl ClientTlsConfig {
    /// Creates a new `ClientTlsConfig` using Rustls.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the domain name against which to verify the server's TLS certificate.
    pub fn domain_name(self, domain_name: impl Into<String>) -> Self {
        ClientTlsConfig {
            domain: Some(domain_name.into()),
            ..self
        }
    }

    /// Adds the CA Certificate against which to verify the server's TLS certificate.
    pub fn ca_certificate(self, ca_certificate: Certificate) -> Self {
        let mut certs = self.certs;
        certs.push(ca_certificate);
        ClientTlsConfig { certs, ..self }
    }

    /// Adds the multiple CA Certificates against which to verify the server's TLS certificate.
    pub fn ca_certificates(self, ca_certificates: impl IntoIterator<Item = Certificate>) -> Self {
        let mut certs = self.certs;
        certs.extend(ca_certificates);
        ClientTlsConfig { certs, ..self }
    }

    /// Adds the trust anchor which to verify the server's TLS certificate.
    pub fn trust_anchor(self, trust_anchor: TrustAnchor<'static>) -> Self {
        let mut trust_anchors = self.trust_anchors;
        trust_anchors.push(trust_anchor);
        ClientTlsConfig {
            trust_anchors,
            ..self
        }
    }

    /// Adds the multiple trust anchors which to verify the server's TLS certificate.
    pub fn trust_anchors(
        mut self,
        trust_anchors: impl IntoIterator<Item = TrustAnchor<'static>>,
    ) -> Self {
        self.trust_anchors.extend(trust_anchors);
        self
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

    /// Use key log as specified by the `SSLKEYLOGFILE` environment variable.
    pub fn use_key_log(self) -> Self {
        ClientTlsConfig {
            use_key_log: true,
            ..self
        }
    }

    /// Enables the platform's trusted certs.
    #[cfg(feature = "tls-native-roots")]
    pub fn with_native_roots(self) -> Self {
        ClientTlsConfig {
            with_native_roots: true,
            ..self
        }
    }

    /// Enables the webpki roots.
    #[cfg(feature = "tls-webpki-roots")]
    pub fn with_webpki_roots(self) -> Self {
        ClientTlsConfig {
            with_webpki_roots: true,
            ..self
        }
    }

    /// Activates all TLS roots enabled through `tls-*-roots` feature flags
    pub fn with_enabled_roots(self) -> Self {
        let config = self;

        #[cfg(feature = "tls-native-roots")]
        let config = config.with_native_roots();
        #[cfg(feature = "tls-webpki-roots")]
        let config = config.with_webpki_roots();

        config
    }

    /// Sets the timeout for the TLS handshake.
    pub fn timeout(self, timeout: Duration) -> Self {
        ClientTlsConfig {
            timeout: Some(timeout),
            ..self
        }
    }

    /// Replaces the default WebPKI server certificate verifier with a custom
    /// implementation.
    ///
    /// **Warning:** A misconfigured verifier can silently disable peer
    /// validation. Only use this if you understand rustls' verifier contract.
    ///
    /// Cannot be combined with [`ca_certificate`](Self::ca_certificate),
    /// [`ca_certificates`](Self::ca_certificates),
    /// [`trust_anchor`](Self::trust_anchor),
    /// [`trust_anchors`](Self::trust_anchors),
    /// `with_native_roots`, `with_webpki_roots`, or
    /// [`with_enabled_roots`](Self::with_enabled_roots) — those configure the
    /// default verifier, which is replaced when a custom one is set. Mixing
    /// produces an error at connector-construction time.
    ///
    /// SNI ([`domain_name`](Self::domain_name)), client identity
    /// ([`identity`](Self::identity)), [`timeout`](Self::timeout),
    /// [`use_key_log`](Self::use_key_log), and
    /// [`assume_http2`](Self::assume_http2) continue to apply.
    pub fn server_cert_verifier(self, verifier: Arc<dyn ServerCertVerifier>) -> Self {
        ClientTlsConfig {
            server_cert_verifier: Some(verifier),
            ..self
        }
    }

    pub(crate) fn into_tls_connector(self, uri: &Uri) -> Result<TlsConnector, crate::BoxError> {
        let domain = match &self.domain {
            Some(domain) => domain,
            None => uri.host().ok_or_else(Error::new_invalid_uri)?,
        };
        TlsConnector::new(
            self.certs,
            self.trust_anchors,
            self.identity,
            self.server_cert_verifier,
            domain,
            self.assume_http2,
            self.use_key_log,
            self.timeout,
            #[cfg(feature = "tls-native-roots")]
            self.with_native_roots,
            #[cfg(feature = "tls-webpki-roots")]
            self.with_webpki_roots,
        )
    }
}
