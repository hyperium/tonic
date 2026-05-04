//! Custom rustls [`ServerCertVerifier`] with gRFC A29 SAN matching.
//!
//! Chain validation (signature, expiry, revocation) is delegated to rustls'
//! built-in [`WebPkiServerVerifier`]. After a chain passes, we additionally
//! extract the leaf cert's SAN extension with [`x509_parser`] and run the
//! cluster's [`SanMatcher`] list using "any" semantics per the xDS
//! `CertificateValidationContext` contract (match succeeds if any matcher
//! matches any SAN entry).
//!
//! [`ServerCertVerifier`]: rustls::client::danger::ServerCertVerifier
//! [`WebPkiServerVerifier`]: rustls::client::WebPkiServerVerifier

use std::net::IpAddr;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as RustlsError, RootCertStore, SignatureScheme,
};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::oid_registry::OID_X509_EXT_SUBJECT_ALT_NAME;
use x509_parser::prelude::FromDer;

use crate::xds::cert_provider::{CertProviderError, CertProviderRegistry, CertificateData};
use crate::xds::resource::san_matcher::{SanEntry, SanMatcher};
use crate::xds::resource::security::ClusterSecurityConfig;

/// Verifier that wraps [`WebPkiServerVerifier`] and enforces gRFC A29 SAN
/// matching after the WebPKI chain check passes.
#[derive(Debug)]
pub(crate) struct XdsServerCertVerifier {
    inner: Arc<WebPkiServerVerifier>,
    san_matchers: Vec<SanMatcher>,
}

impl XdsServerCertVerifier {
    pub(crate) fn new(
        roots: RootCertStore,
        san_matchers: Vec<SanMatcher>,
    ) -> Result<Self, rustls::client::VerifierBuilderError> {
        let inner = WebPkiServerVerifier::builder(Arc::new(roots)).build()?;
        Ok(Self {
            inner,
            san_matchers,
        })
    }
}

impl ServerCertVerifier for XdsServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;

        // A29 SAN matching uses "any" semantics: at least one matcher must match
        // some SAN entry. Empty matcher list means CA-trust-only authorization.
        if !self.san_matchers.is_empty() {
            let sans = extract_sans(end_entity)
                .map_err(|e| RustlsError::General(format!("failed to extract SANs: {e}")))?;
            if !self.san_matchers.iter().any(|m| m.matches_any(&sans)) {
                return Err(RustlsError::General(
                    "no SAN matcher matched the presented certificate".into(),
                ));
            }
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Extract SAN entries from an X.509 cert's subjectAltName extension.
///
/// Unsupported GeneralName variants (DirectoryName, X400Address, etc.) and
/// malformed IP addresses are silently skipped — they can't contribute to a
/// match regardless.
pub(crate) fn extract_sans(der: &CertificateDer<'_>) -> Result<Vec<SanEntry>, String> {
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(der.as_ref())
        .map_err(|e| format!("x509 parse error: {e}"))?;

    let Some(san_ext) = cert
        .tbs_certificate
        .extensions()
        .iter()
        .find(|ext| ext.oid == OID_X509_EXT_SUBJECT_ALT_NAME)
    else {
        return Ok(Vec::new());
    };

    let ParsedExtension::SubjectAlternativeName(san) = san_ext.parsed_extension() else {
        return Ok(Vec::new());
    };

    Ok(san
        .general_names
        .iter()
        .filter_map(convert_general_name)
        .collect())
}

fn convert_general_name(gn: &GeneralName<'_>) -> Option<SanEntry> {
    match gn {
        GeneralName::DNSName(s) => Some(SanEntry::Dns(s.to_string())),
        GeneralName::URI(s) => Some(SanEntry::Uri(s.to_string())),
        GeneralName::RFC822Name(s) => Some(SanEntry::Email(s.to_string())),
        GeneralName::IPAddress(bytes) => parse_ip_san(bytes).map(SanEntry::IpAddress),
        GeneralName::OtherName(oid, value) => Some(SanEntry::OtherName {
            oid: oid.to_id_string(),
            value: value.to_vec(),
        }),
        _ => None,
    }
}

fn parse_ip_san(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(IpAddr::from),
        16 => <[u8; 16]>::try_from(bytes).ok().map(IpAddr::from),
        _ => None,
    }
}

/// Errors building a [`rustls::ClientConfig`] from a cluster's security config.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientConfigError {
    /// Provider lookup or contents (missing roots/identity, unknown instance).
    #[error("provider: {0}")]
    Provider(String),
    /// Provider failed to fetch certificate material.
    #[error("provider fetch: {0}")]
    Fetch(#[from] CertProviderError),
    /// PEM parsing failed or yielded no usable cert/key.
    #[error("pem: {0}")]
    Pem(String),
    /// rustls rejected the supplied verifier or client-auth identity.
    #[error("rustls: {0}")]
    Rustls(String),
}

/// Build a [`rustls::ClientConfig`] for a cluster from its [`ClusterSecurityConfig`]
/// and the bootstrap [`CertProviderRegistry`].
///
/// Resolves the CA + (optional) identity provider instances by name, fetches
/// the certificate material, builds an [`XdsServerCertVerifier`] with the
/// cluster's SAN matchers, and assembles a `ClientConfig` with the custom
/// verifier installed via `.dangerous().with_custom_certificate_verifier(...)`.
///
/// The returned config is ready to be plugged into a TLS connector once an
/// upstream API exists for that — see the TODO in
/// [`crate::xds::cluster_discovery::build_connector`].
pub(crate) fn build_client_config(
    registry: &CertProviderRegistry,
    security: &ClusterSecurityConfig,
) -> Result<ClientConfig, ClientConfigError> {
    let ca_data = fetch_provider_data(registry, &security.ca_instance_name, "CA")?;
    let ca_pem = ca_data.roots().ok_or_else(|| {
        ClientConfigError::Provider(format!(
            "CA instance '{}' has no roots",
            security.ca_instance_name
        ))
    })?;
    let root_store = build_root_store(ca_pem)?;

    let verifier = Arc::new(
        XdsServerCertVerifier::new(root_store, security.san_matchers.clone())
            .map_err(|e| ClientConfigError::Rustls(e.to_string()))?,
    );

    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    let config = match &security.identity_instance_name {
        Some(name) => {
            let id_data = fetch_provider_data(registry, name, "identity")?;
            let identity = id_data.identity().ok_or_else(|| {
                ClientConfigError::Provider(format!("identity instance '{name}' has no identity"))
            })?;
            let cert_chain = parse_pem_certs(&identity.cert_chain)?;
            let key = parse_pem_key(&identity.key)?;
            builder
                .with_client_auth_cert(cert_chain, key)
                .map_err(|e| ClientConfigError::Rustls(e.to_string()))?
        }
        None => builder.with_no_client_auth(),
    };

    Ok(config)
}

fn fetch_provider_data(
    registry: &CertProviderRegistry,
    name: &str,
    role: &str,
) -> Result<Arc<CertificateData>, ClientConfigError> {
    let provider = registry
        .get(name)
        .ok_or_else(|| ClientConfigError::Provider(format!("unknown {role} instance '{name}'")))?;
    Ok(provider.fetch()?)
}

fn build_root_store(pem: &[u8]) -> Result<RootCertStore, ClientConfigError> {
    let certs = parse_pem_certs(pem)?;
    let mut store = RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(ClientConfigError::Pem("no certificates in PEM".into()));
    }
    Ok(store)
}

fn parse_pem_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, ClientConfigError> {
    let mut reader = std::io::Cursor::new(pem);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<_, _>>()
        .map_err(|e| ClientConfigError::Pem(e.to_string()))
}

fn parse_pem_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, ClientConfigError> {
    let mut reader = std::io::Cursor::new(pem);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| ClientConfigError::Pem(e.to_string()))?
        .ok_or_else(|| ClientConfigError::Pem("no private key in PEM".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, SanType as RcgenSanType};

    /// Generate a self-signed DER cert carrying the given SANs.
    fn gen_cert_with_sans(sans: Vec<RcgenSanType>) -> CertificateDer<'static> {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = sans;
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        cert.der().clone()
    }

    #[test]
    fn extract_sans_empty_cert() {
        let der = gen_cert_with_sans(vec![]);
        let sans = extract_sans(&der).unwrap();
        assert!(sans.is_empty());
    }

    #[test]
    fn extract_sans_dns() {
        let der = gen_cert_with_sans(vec![RcgenSanType::DnsName(
            "api.example.com".try_into().unwrap(),
        )]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(sans, vec![SanEntry::Dns("api.example.com".into())]);
    }

    #[test]
    fn extract_sans_uri() {
        let der = gen_cert_with_sans(vec![RcgenSanType::URI(
            "spiffe://trust/ns/prod/sa/api".try_into().unwrap(),
        )]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(
            sans,
            vec![SanEntry::Uri("spiffe://trust/ns/prod/sa/api".into())]
        );
    }

    #[test]
    fn extract_sans_email() {
        let der = gen_cert_with_sans(vec![RcgenSanType::Rfc822Name(
            "svc@corp.test".try_into().unwrap(),
        )]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(sans, vec![SanEntry::Email("svc@corp.test".into())]);
    }

    #[test]
    fn extract_sans_ipv4() {
        let der = gen_cert_with_sans(vec![RcgenSanType::IpAddress(
            "192.168.1.5".parse().unwrap(),
        )]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(
            sans,
            vec![SanEntry::IpAddress("192.168.1.5".parse().unwrap())]
        );
    }

    #[test]
    fn extract_sans_ipv6() {
        let der = gen_cert_with_sans(vec![RcgenSanType::IpAddress(
            "2001:db8::1".parse().unwrap(),
        )]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(
            sans,
            vec![SanEntry::IpAddress("2001:db8::1".parse().unwrap())]
        );
    }

    #[test]
    fn extract_sans_multiple_mixed_types() {
        let der = gen_cert_with_sans(vec![
            RcgenSanType::DnsName("api.example.com".try_into().unwrap()),
            RcgenSanType::URI("spiffe://trust/ns/prod/sa/api".try_into().unwrap()),
            RcgenSanType::IpAddress("10.0.0.1".parse().unwrap()),
        ]);
        let sans = extract_sans(&der).unwrap();
        assert_eq!(sans.len(), 3);
        assert!(sans.contains(&SanEntry::Dns("api.example.com".into())));
        assert!(sans.contains(&SanEntry::Uri("spiffe://trust/ns/prod/sa/api".into())));
        assert!(sans.contains(&SanEntry::IpAddress("10.0.0.1".parse().unwrap())));
    }

    #[test]
    fn extract_sans_malformed_der_errors() {
        let der = CertificateDer::from(vec![0x00, 0x01, 0x02]);
        let err = extract_sans(&der).unwrap_err();
        assert!(err.contains("x509 parse error"));
    }
}
