//! Custom rustls [`ServerCertVerifier`] for gRFC A29.
//!
//! Chain validation uses rustls'
//! [`verify_server_cert_signed_by_trust_anchor`]. The xDS SAN matcher list
//! then runs against the leaf cert's SAN entries with "any" semantics — a
//! match succeeds when any matcher matches any SAN entry. An empty matcher
//! list accepts any cert chained to the configured roots.
//!
//! [`ServerCertVerifier`]: rustls::client::danger::ServerCertVerifier
//! [`verify_server_cert_signed_by_trust_anchor`]: rustls::client::verify_server_cert_signed_by_trust_anchor

use std::net::IpAddr;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::verify_server_cert_signed_by_trust_anchor;
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::oid_registry::OID_X509_EXT_SUBJECT_ALT_NAME;
use x509_parser::prelude::FromDer;

use crate::xds::cert_provider::CertificateProvider;
use crate::xds::resource::san_matcher::{SanEntry, SanMatcher};

/// Verifier that chain-validates the peer cert and enforces gRFC A29 SAN
/// matching. Sources CA roots from a [`CertificateProvider`] per handshake
/// so cert rotation in the provider is picked up automatically.
pub(crate) struct XdsServerCertVerifier {
    ca_provider: Arc<dyn CertificateProvider>,
    supported_algs: WebPkiSupportedAlgorithms,
    san_matchers: Vec<SanMatcher>,
}

impl std::fmt::Debug for XdsServerCertVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdsServerCertVerifier")
            .field("san_matchers", &self.san_matchers)
            .finish_non_exhaustive()
    }
}

impl XdsServerCertVerifier {
    pub(crate) fn new(
        ca_provider: Arc<dyn CertificateProvider>,
        san_matchers: Vec<SanMatcher>,
    ) -> Self {
        let provider = default_crypto_provider();
        Self {
            ca_provider,
            supported_algs: provider.signature_verification_algorithms,
            san_matchers,
        }
    }
}

/// Resolve a [`rustls::crypto::CryptoProvider`]: prefer the process-installed
/// default, fall back to a feature-flagged provider. Mirrors tonic's
/// `transport::channel::service::tls` bootstrap so we make the same choice as
/// the rest of the channel stack.
fn default_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    if let Some(p) = rustls::crypto::CryptoProvider::get_default() {
        return p.clone();
    }
    #[cfg(feature = "tls-ring")]
    return Arc::new(rustls::crypto::ring::default_provider());
    #[cfg(all(not(feature = "tls-ring"), feature = "tls-aws-lc"))]
    return Arc::new(rustls::crypto::aws_lc_rs::default_provider());
}

impl ServerCertVerifier for XdsServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // `server_name` is intentionally unused — gRFC A29 replaces stdlib
        // hostname verification with the SAN matcher list below.
        let data = self
            .ca_provider
            .fetch()
            .map_err(|e| RustlsError::General(format!("CA provider fetch failed: {e}")))?;
        let roots = data
            .roots()
            .ok_or_else(|| RustlsError::General("CA provider has no roots".into()))?;

        let cert = ParsedCertificate::try_from(end_entity)?;
        verify_server_cert_signed_by_trust_anchor(
            &cert,
            roots,
            intermediates,
            now,
            self.supported_algs.all,
        )?;

        // A29 SAN matching uses "any" semantics: at least one matcher must match
        // some SAN entry. Empty matcher list means CA-trust-only authorization.
        if !self.san_matchers.is_empty() {
            let sans = extract_sans(end_entity)
                .map_err(|e| RustlsError::General(format!("failed to extract SANs: {e}")))?;
            if !self.san_matchers.iter().any(|m| m.matches_any(&sans)) {
                return Err(RustlsError::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
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
        verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

/// Extract SAN entries from an X.509 cert's subjectAltName extension.
///
/// Only the four gRFC A29-defined SAN types (DNS, URI, EMAIL, IP) are surfaced.
/// Other GeneralName variants (DirectoryName, X400Address, OtherName, ...)
/// are dropped — see [`convert_general_name`].
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

/// Return a [`SanEntry`] for DNS, URI, EMAIL, or IP_ADDRESS SANs — the four
/// types gRFC A29 enforces matching for.
/// Return `None` for other GeneralName
/// variants (DirectoryName, X400Address, OtherName, ...).
fn convert_general_name(gn: &GeneralName<'_>) -> Option<SanEntry> {
    match gn {
        GeneralName::DNSName(s) => Some(SanEntry::Dns(s.to_string())),
        GeneralName::URI(s) => Some(SanEntry::Uri(s.to_string())),
        GeneralName::RFC822Name(s) => Some(SanEntry::Email(s.to_string())),
        GeneralName::IPAddress(bytes) => parse_ip_san(bytes).map(SanEntry::IpAddress),
        _ => None,
    }
}

/// Parse a SAN iPAddress: exactly 4 octets (IPv4) or 16 (IPv6) per
/// RFC 5280 §4.2.1.6.
fn parse_ip_san(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(IpAddr::from),
        16 => <[u8; 16]>::try_from(bytes).ok().map(IpAddr::from),
        // The 8/32-byte `<addr><mask>` form applies to nameConstraints
        // (§4.2.1.10), not SAN.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::cert_provider::{CertProviderError, CertificateData, Identity};
    use rcgen::{CertificateParams, SanType as RcgenSanType};
    use rustls::RootCertStore;

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

    /// Build a small chain: a self-signed CA and a leaf signed by it that
    /// carries only a `URI` SAN (SPIFFE-style). Returns `(ca_der, leaf_der)`.
    fn build_chain_with_spiffe_leaf(
        spiffe_uri: &str,
    ) -> (CertificateDer<'static>, CertificateDer<'static>) {
        use rcgen::{BasicConstraints, IsCa, Issuer, KeyPair, KeyUsagePurpose};

        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec!["test-ca".into()]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_der = ca_cert.der().clone();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        leaf_params.subject_alt_names = vec![RcgenSanType::URI(spiffe_uri.try_into().unwrap())];
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
        let leaf_der = leaf_cert.der().clone();

        (ca_der, leaf_der)
    }

    fn root_store_with(ca_der: CertificateDer<'static>) -> RootCertStore {
        let mut store = RootCertStore::empty();
        store.add(ca_der).unwrap();
        store
    }

    /// Test shim: a [`CertificateProvider`] that returns a fixed snapshot.
    struct StaticProvider(Arc<CertificateData>);

    impl CertificateProvider for StaticProvider {
        fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
            Ok(self.0.clone())
        }
    }

    fn provider_with_roots(store: RootCertStore) -> Arc<dyn CertificateProvider> {
        Arc::new(StaticProvider(Arc::new(CertificateData::RootsOnly {
            roots: Arc::new(store),
        })))
    }

    fn uri_matcher(spiffe_uri: &str) -> SanMatcher {
        use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::{
            SubjectAltNameMatcher, subject_alt_name_matcher::SanType,
        };
        use envoy_types::pb::envoy::r#type::matcher::v3::StringMatcher as StringMatcherProto;
        use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;
        SanMatcher::from_proto(SubjectAltNameMatcher {
            san_type: SanType::Uri as i32,
            matcher: Some(StringMatcherProto {
                match_pattern: Some(MatchPattern::Exact(spiffe_uri.into())),
                ignore_case: false,
            }),
            oid: String::new(),
        })
        .unwrap()
    }

    #[test]
    fn spiffe_uri_only_cert_with_matching_uri_matcher_passes() {
        let (ca_der, leaf_der) = build_chain_with_spiffe_leaf("spiffe://td/ns/prod/sa/api");
        let verifier = XdsServerCertVerifier::new(
            provider_with_roots(root_store_with(ca_der)),
            vec![uri_matcher("spiffe://td/ns/prod/sa/api")],
        );

        let server_name = ServerName::try_from("any.connect.hostname").unwrap();
        let result =
            verifier.verify_server_cert(&leaf_der, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[test]
    fn spiffe_uri_only_cert_with_non_matching_matcher_fails() {
        let (ca_der, leaf_der) = build_chain_with_spiffe_leaf("spiffe://td/ns/prod/sa/api");
        let verifier = XdsServerCertVerifier::new(
            provider_with_roots(root_store_with(ca_der)),
            vec![uri_matcher("spiffe://td/ns/prod/sa/other")],
        );

        let server_name = ServerName::try_from("any.connect.hostname").unwrap();
        let err = verifier
            .verify_server_cert(&leaf_der, &[], &server_name, &[], UnixTime::now())
            .unwrap_err();
        assert!(matches!(
            err,
            RustlsError::InvalidCertificate(CertificateError::ApplicationVerificationFailure),
        ));
    }

    #[test]
    fn spiffe_uri_only_cert_with_empty_matchers_passes_ca_only() {
        // per gRFC A29 §'Server Authorization': an empty matcher list passes
        let (ca_der, leaf_der) = build_chain_with_spiffe_leaf("spiffe://td/ns/prod/sa/api");
        let verifier =
            XdsServerCertVerifier::new(provider_with_roots(root_store_with(ca_der)), vec![]);

        let server_name = ServerName::try_from("any.connect.hostname").unwrap();
        let result =
            verifier.verify_server_cert(&leaf_der, &[], &server_name, &[], UnixTime::now());
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[test]
    fn verify_fails_when_provider_has_no_roots() {
        let (_ca_der, leaf_der) = build_chain_with_spiffe_leaf("spiffe://td/ns/prod/sa/api");
        let provider: Arc<dyn CertificateProvider> =
            Arc::new(StaticProvider(Arc::new(CertificateData::IdentityOnly {
                identity: Identity {
                    cert_chain: b"chain".to_vec(),
                    key: b"key".to_vec(),
                },
            })));
        let verifier = XdsServerCertVerifier::new(provider, vec![]);

        let server_name = ServerName::try_from("any.connect.hostname").unwrap();
        let err = verifier
            .verify_server_cert(&leaf_der, &[], &server_name, &[], UnixTime::now())
            .unwrap_err();
        assert!(
            matches!(err, RustlsError::General(ref msg) if msg.contains("no roots")),
            "expected General(\"...no roots...\"), got {err:?}",
        );
    }
}
