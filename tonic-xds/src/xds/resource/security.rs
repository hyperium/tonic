//! Cluster-level TLS security config parsing (gRFC A29).
//!
//! Parses `Cluster.transport_socket.typed_config` as an `UpstreamTlsContext`
//! per A29 rules: only certificate-provider-instance based config is supported
//! (no inline certs, no SDS), and the trust anchor must be resolved through
//! the bootstrap `certificate_providers` registry.

use envoy_types::pb::envoy::config::core::v3::TransportSocket;
use envoy_types::pb::envoy::config::core::v3::transport_socket::ConfigType as TransportSocketConfigType;
use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::{
    CertificateValidationContext, CommonTlsContext, UpstreamTlsContext, common_tls_context,
};
use prost::{Message, Name};
use xds_client::Error;

use super::san_matcher::SanMatcher;
use super::string_matcher::StringMatcher;

const TLS_TRANSPORT_SOCKET_NAME: &str = "envoy.transport_sockets.tls";

/// Cluster-level TLS security config.
///
/// Holds the instance names referenced by the cluster, not resolved
/// providers. Resolution against [`CertProviderRegistry`] happens later, at
/// connection-building time, so that this type can be derived during CDS
/// resource validation (where the registry is not available).
///
/// [`CertProviderRegistry`]: crate::xds::cert_provider::CertProviderRegistry
#[derive(Debug, Clone)]
pub(crate) struct ClusterSecurityConfig {
    /// Bootstrap instance name for the CA trust bundle. Required.
    pub ca_instance_name: String,
    /// Bootstrap instance name for client identity. `Some` implies mTLS.
    pub identity_instance_name: Option<String>,
    /// SAN matchers for server authorization. May be empty.
    pub san_matchers: Vec<SanMatcher>,
}

/// Parse a cluster's `transport_socket` into a [`ClusterSecurityConfig`].
///
/// Returns `Ok(None)` when `transport_socket` is absent (plaintext cluster).
/// Returns `Err(Validation)` for any A29 NACK condition.
pub(crate) fn parse_transport_socket(
    transport_socket: Option<TransportSocket>,
) -> xds_client::Result<Option<ClusterSecurityConfig>> {
    let Some(ts) = transport_socket else {
        return Ok(None);
    };

    if ts.name != TLS_TRANSPORT_SOCKET_NAME {
        return Err(Error::Validation(format!(
            "unsupported transport_socket '{}': only '{TLS_TRANSPORT_SOCKET_NAME}' is supported",
            ts.name
        )));
    }

    let Some(TransportSocketConfigType::TypedConfig(any)) = ts.config_type else {
        return Err(Error::Validation(
            "transport_socket missing typed_config".into(),
        ));
    };

    if any.type_url != UpstreamTlsContext::type_url() {
        return Err(Error::Validation(format!(
            "transport_socket typed_config type_url '{}' does not match UpstreamTlsContext",
            any.type_url
        )));
    }

    let upstream = UpstreamTlsContext::decode(any.value.as_slice())
        .map_err(|e| Error::Validation(format!("failed to decode UpstreamTlsContext: {e}")))?;

    let common = upstream
        .common_tls_context
        .ok_or_else(|| Error::Validation("UpstreamTlsContext missing common_tls_context".into()))?;

    parse_common_tls_context(common).map(Some)
}

fn parse_common_tls_context(ctx: CommonTlsContext) -> xds_client::Result<ClusterSecurityConfig> {
    reject_unsupported_common_fields(&ctx)?;

    // Identity (mTLS client cert) — optional.
    let identity_instance_name = ctx
        .tls_certificate_provider_instance
        .map(|pi| pi.instance_name)
        .filter(|s| !s.is_empty());

    let validation_ctx = resolve_validation_context(ctx.validation_context_type)?;
    reject_unsupported_validation_fields(&validation_ctx)?;
    let san_matchers = parse_san_matchers(&validation_ctx)?;

    // CA trust anchor — required per A29.
    let ca = validation_ctx
        .ca_certificate_provider_instance
        .ok_or_else(|| {
            Error::Validation(
                "CertificateValidationContext missing ca_certificate_provider_instance".into(),
            )
        })?;
    if ca.instance_name.is_empty() {
        return Err(Error::Validation(
            "ca_certificate_provider_instance.instance_name is empty".into(),
        ));
    }

    Ok(ClusterSecurityConfig {
        ca_instance_name: ca.instance_name,
        identity_instance_name,
        san_matchers,
    })
}

fn resolve_validation_context(
    vct: Option<common_tls_context::ValidationContextType>,
) -> xds_client::Result<CertificateValidationContext> {
    use common_tls_context::ValidationContextType;
    match vct {
        Some(ValidationContextType::ValidationContext(ctx)) => Ok(ctx),
        Some(ValidationContextType::CombinedValidationContext(combined)) => {
            combined.default_validation_context.ok_or_else(|| {
                Error::Validation(
                    "CombinedValidationContext missing default_validation_context".into(),
                )
            })
        }
        // SDS-based validation is not supported in A29.
        Some(ValidationContextType::ValidationContextSdsSecretConfig(_)) => Err(Error::Validation(
            "SDS-based validation_context is not supported".into(),
        )),
        // Deprecated variants.
        Some(_) => Err(Error::Validation(
            "unsupported validation_context_type variant".into(),
        )),
        None => Err(Error::Validation(
            "CommonTlsContext missing validation_context_type".into(),
        )),
    }
}

/// SAN matchers: typed wins per envoy proto semantics. Falls back to the
/// deprecated DNS-only `match_subject_alt_names` when typed is empty.
fn parse_san_matchers(ctx: &CertificateValidationContext) -> xds_client::Result<Vec<SanMatcher>> {
    if !ctx.match_typed_subject_alt_names.is_empty() {
        return ctx
            .match_typed_subject_alt_names
            .iter()
            .cloned()
            .map(SanMatcher::from_proto)
            .collect();
    }
    #[allow(deprecated)]
    ctx.match_subject_alt_names
        .iter()
        .cloned()
        .map(|m| StringMatcher::from_proto(m).map(SanMatcher::Dns))
        .collect()
}

/// Reject `CommonTlsContext` fields we don't support to avoid silently
/// degrading security relative to what the config requested.
fn reject_unsupported_common_fields(ctx: &CommonTlsContext) -> xds_client::Result<()> {
    reject(ctx.tls_params.is_some(), "CommonTlsContext.tls_params")?;
    reject(
        !ctx.tls_certificates.is_empty(),
        "CommonTlsContext.tls_certificates",
    )?;
    reject(
        !ctx.tls_certificate_sds_secret_configs.is_empty(),
        "CommonTlsContext.tls_certificate_sds_secret_configs",
    )?;
    reject(
        ctx.custom_handshaker.is_some(),
        "CommonTlsContext.custom_handshaker",
    )?;
    #[allow(deprecated)]
    {
        reject(
            ctx.tls_certificate_certificate_provider.is_some(),
            "CommonTlsContext.tls_certificate_certificate_provider",
        )?;
        reject(
            ctx.tls_certificate_certificate_provider_instance.is_some(),
            "CommonTlsContext.tls_certificate_certificate_provider_instance",
        )?;
    }
    Ok(())
}

/// Reject `CertificateValidationContext` fields whose silent absence would
/// weaken peer verification relative to what the config requested.
fn reject_unsupported_validation_fields(
    ctx: &CertificateValidationContext,
) -> xds_client::Result<()> {
    reject(
        !ctx.verify_certificate_spki.is_empty(),
        "verify_certificate_spki",
    )?;
    reject(
        !ctx.verify_certificate_hash.is_empty(),
        "verify_certificate_hash",
    )?;
    reject(
        ctx.require_signed_certificate_timestamp.is_some(),
        "require_signed_certificate_timestamp",
    )?;
    reject(ctx.crl.is_some(), "crl")?;
    Ok(())
}

fn reject(set: bool, field: &str) -> xds_client::Result<()> {
    if set {
        Err(Error::Validation(format!("{field} is not supported")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::{
        CertificateProviderPluginInstance, SubjectAltNameMatcher, subject_alt_name_matcher::SanType,
    };
    use envoy_types::pb::envoy::r#type::matcher::v3::StringMatcher as StringMatcherProto;
    use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;
    use envoy_types::pb::google::protobuf::Any;

    fn provider_instance(name: &str) -> CertificateProviderPluginInstance {
        CertificateProviderPluginInstance {
            instance_name: name.into(),
            certificate_name: String::new(),
        }
    }

    fn ca_validation_ctx(ca: &str) -> CertificateValidationContext {
        CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance(ca)),
            ..Default::default()
        }
    }

    fn common_ctx(cvc: CertificateValidationContext) -> CommonTlsContext {
        CommonTlsContext {
            validation_context_type: Some(
                common_tls_context::ValidationContextType::ValidationContext(cvc),
            ),
            ..Default::default()
        }
    }

    fn wrap_upstream(common: CommonTlsContext) -> TransportSocket {
        let upstream = UpstreamTlsContext {
            common_tls_context: Some(common),
            ..Default::default()
        };
        TransportSocket {
            name: TLS_TRANSPORT_SOCKET_NAME.into(),
            config_type: Some(TransportSocketConfigType::TypedConfig(Any {
                type_url: UpstreamTlsContext::type_url(),
                value: upstream.encode_to_vec(),
            })),
        }
    }

    #[test]
    fn absent_transport_socket_yields_plaintext() {
        let res = parse_transport_socket(None).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn basic_ca_only_tls() {
        let ts = wrap_upstream(common_ctx(ca_validation_ctx("root_ca")));
        let cfg = parse_transport_socket(Some(ts)).unwrap().unwrap();
        assert_eq!(cfg.ca_instance_name, "root_ca");
        assert!(cfg.identity_instance_name.is_none());
        assert!(cfg.san_matchers.is_empty());
    }

    #[test]
    fn mtls_with_identity() {
        let common = CommonTlsContext {
            tls_certificate_provider_instance: Some(provider_instance("client_id")),
            validation_context_type: Some(
                common_tls_context::ValidationContextType::ValidationContext(ca_validation_ctx(
                    "root_ca",
                )),
            ),
            ..Default::default()
        };
        let ts = wrap_upstream(common);
        let cfg = parse_transport_socket(Some(ts)).unwrap().unwrap();
        assert_eq!(cfg.ca_instance_name, "root_ca");
        assert_eq!(cfg.identity_instance_name.as_deref(), Some("client_id"));
    }

    #[test]
    fn combined_validation_context_is_supported() {
        let common = CommonTlsContext {
            validation_context_type: Some(
                common_tls_context::ValidationContextType::CombinedValidationContext(
                    common_tls_context::CombinedCertificateValidationContext {
                        default_validation_context: Some(ca_validation_ctx("root_ca")),
                        ..Default::default()
                    },
                ),
            ),
            ..Default::default()
        };
        let cfg = parse_transport_socket(Some(wrap_upstream(common)))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.ca_instance_name, "root_ca");
    }

    #[test]
    fn typed_san_matchers_parsed() {
        let sm = StringMatcherProto {
            match_pattern: Some(MatchPattern::Exact("api.example.com".into())),
            ignore_case: false,
        };
        let san = SubjectAltNameMatcher {
            san_type: SanType::Dns as i32,
            matcher: Some(sm),
            oid: String::new(),
        };
        let cvc = CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance("root_ca")),
            match_typed_subject_alt_names: vec![san],
            ..Default::default()
        };
        let cfg = parse_transport_socket(Some(wrap_upstream(common_ctx(cvc))))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.san_matchers.len(), 1);
        assert!(matches!(cfg.san_matchers[0], SanMatcher::Dns(_)));
    }

    #[test]
    fn typed_sans_take_precedence_over_legacy() {
        let typed = SubjectAltNameMatcher {
            san_type: SanType::Uri as i32,
            matcher: Some(StringMatcherProto {
                match_pattern: Some(MatchPattern::Exact("spiffe://typed".into())),
                ignore_case: false,
            }),
            oid: String::new(),
        };
        #[allow(deprecated)]
        let cvc = CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance("root_ca")),
            match_typed_subject_alt_names: vec![typed],
            match_subject_alt_names: vec![StringMatcherProto {
                match_pattern: Some(MatchPattern::Exact("legacy.example.com".into())),
                ignore_case: false,
            }],
            ..Default::default()
        };
        let cfg = parse_transport_socket(Some(wrap_upstream(common_ctx(cvc))))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.san_matchers.len(), 1);
        // Should be the URI matcher (typed), not DNS (legacy).
        assert!(matches!(cfg.san_matchers[0], SanMatcher::Uri(_)));
    }

    #[test]
    fn legacy_sans_used_when_typed_empty() {
        #[allow(deprecated)]
        let cvc = CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance("root_ca")),
            match_subject_alt_names: vec![StringMatcherProto {
                match_pattern: Some(MatchPattern::Exact("legacy.example.com".into())),
                ignore_case: false,
            }],
            ..Default::default()
        };
        let cfg = parse_transport_socket(Some(wrap_upstream(common_ctx(cvc))))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.san_matchers.len(), 1);
        // Legacy field treats entries as DNS SAN matchers.
        assert!(matches!(cfg.san_matchers[0], SanMatcher::Dns(_)));
    }

    #[test]
    fn wrong_transport_socket_name_is_rejected() {
        let ts = TransportSocket {
            name: "envoy.transport_sockets.raw_buffer".into(),
            config_type: None,
        };
        let err = parse_transport_socket(Some(ts)).unwrap_err();
        assert!(err.to_string().contains("unsupported transport_socket"));
    }

    #[test]
    fn wrong_type_url_is_rejected() {
        let ts = TransportSocket {
            name: TLS_TRANSPORT_SOCKET_NAME.into(),
            config_type: Some(TransportSocketConfigType::TypedConfig(Any {
                type_url: "type.googleapis.com/something.else".into(),
                value: vec![],
            })),
        };
        let err = parse_transport_socket(Some(ts)).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not match UpstreamTlsContext")
        );
    }

    #[test]
    fn missing_validation_context_is_rejected() {
        let common = CommonTlsContext {
            validation_context_type: None,
            ..Default::default()
        };
        let err = parse_transport_socket(Some(wrap_upstream(common))).unwrap_err();
        assert!(err.to_string().contains("missing validation_context_type"));
    }

    #[test]
    fn missing_ca_provider_is_rejected() {
        let ts = wrap_upstream(common_ctx(CertificateValidationContext::default()));
        let err = parse_transport_socket(Some(ts)).unwrap_err();
        assert!(
            err.to_string()
                .contains("missing ca_certificate_provider_instance")
        );
    }

    #[test]
    fn empty_ca_instance_name_is_rejected() {
        let ts = wrap_upstream(common_ctx(ca_validation_ctx("")));
        let err = parse_transport_socket(Some(ts)).unwrap_err();
        assert!(err.to_string().contains("instance_name is empty"));
    }

    #[test]
    fn sds_validation_context_is_rejected() {
        use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::SdsSecretConfig;
        let common = CommonTlsContext {
            validation_context_type: Some(
                common_tls_context::ValidationContextType::ValidationContextSdsSecretConfig(
                    SdsSecretConfig::default(),
                ),
            ),
            ..Default::default()
        };
        let err = parse_transport_socket(Some(wrap_upstream(common))).unwrap_err();
        assert!(err.to_string().contains("SDS"));
    }

    #[test]
    fn inline_tls_certificates_rejected() {
        use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::TlsCertificate;
        let common = CommonTlsContext {
            tls_certificates: vec![TlsCertificate::default()],
            validation_context_type: Some(
                common_tls_context::ValidationContextType::ValidationContext(ca_validation_ctx(
                    "root_ca",
                )),
            ),
            ..Default::default()
        };
        let err = parse_transport_socket(Some(wrap_upstream(common))).unwrap_err();
        assert!(err.to_string().contains("tls_certificates"));
    }

    #[test]
    fn verify_certificate_hash_rejected() {
        let cvc = CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance("root_ca")),
            verify_certificate_hash: vec!["abc".into()],
            ..Default::default()
        };
        let err = parse_transport_socket(Some(wrap_upstream(common_ctx(cvc)))).unwrap_err();
        assert!(err.to_string().contains("verify_certificate_hash"));
    }

    #[test]
    fn verify_certificate_spki_rejected() {
        let cvc = CertificateValidationContext {
            ca_certificate_provider_instance: Some(provider_instance("root_ca")),
            verify_certificate_spki: vec!["abc".into()],
            ..Default::default()
        };
        let err = parse_transport_socket(Some(wrap_upstream(common_ctx(cvc)))).unwrap_err();
        assert!(err.to_string().contains("verify_certificate_spki"));
    }
}
