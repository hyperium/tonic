//! SAN matcher for server authorization (gRFC A29).
//!
//! Wraps the xDS [`SubjectAltNameMatcher`] proto: pairs a SAN type
//! (DNS / URI / EMAIL / IP_ADDRESS / OTHER_NAME) with a [`StringMatcher`].
//! For IP entries A29 specifies converting the cert's IP SAN to its canonical
//! string form (RFC 5952 — lowercase, zero-compressed for IPv6).
//! DNS additionally honors RFC 6125 wildcard rules when the
//! matcher type is `exact` and the cert SAN begins with `*.`.
//!
//! [`SubjectAltNameMatcher`]: envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::SubjectAltNameMatcher

use std::net::IpAddr;

use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::SubjectAltNameMatcher;
use envoy_types::pb::envoy::extensions::transport_sockets::tls::v3::subject_alt_name_matcher::SanType;
use xds_client::Error;

use super::string_matcher::StringMatcher;

/// Validated [`SubjectAltNameMatcher`].
#[derive(Debug, Clone)]
pub(crate) enum SanMatcher {
    Dns(StringMatcher),
    Uri(StringMatcher),
    Email(StringMatcher),
    IpAddress(StringMatcher),
    OtherName { oid: String, matcher: StringMatcher },
}

/// A SAN entry extracted from a peer X.509 certificate.
///
/// Produced by the caller (e.g., a [`rustls::client::danger::ServerCertVerifier`])
/// after parsing the cert's SAN extension. See [`SanMatcher::matches_any`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SanEntry {
    Dns(String),
    Uri(String),
    Email(String),
    IpAddress(IpAddr),
    OtherName { oid: String, value: Vec<u8> },
}

impl SanMatcher {
    pub(crate) fn from_proto(proto: SubjectAltNameMatcher) -> xds_client::Result<Self> {
        let matcher_proto = proto
            .matcher
            .ok_or_else(|| Error::Validation("SubjectAltNameMatcher missing matcher".into()))?;

        let san_type = SanType::try_from(proto.san_type).unwrap_or(SanType::Unspecified);

        match san_type {
            SanType::Dns => Ok(Self::Dns(StringMatcher::from_proto(matcher_proto)?)),
            SanType::Uri => Ok(Self::Uri(StringMatcher::from_proto(matcher_proto)?)),
            SanType::Email => Ok(Self::Email(StringMatcher::from_proto(matcher_proto)?)),
            SanType::IpAddress => Ok(Self::IpAddress(StringMatcher::from_proto(matcher_proto)?)),
            SanType::OtherName => {
                if proto.oid.is_empty() {
                    return Err(Error::Validation(
                        "OTHER_NAME SAN matcher requires 'oid' to be set".into(),
                    ));
                }
                Ok(Self::OtherName {
                    oid: proto.oid,
                    matcher: StringMatcher::from_proto(matcher_proto)?,
                })
            }
            SanType::Unspecified => Err(Error::Validation(
                "SubjectAltNameMatcher san_type is UNSPECIFIED".into(),
            )),
        }
    }

    /// Match succeeds if any entry in `sans` matches this matcher.
    pub(crate) fn matches_any(&self, sans: &[SanEntry]) -> bool {
        sans.iter().any(|entry| self.matches_entry(entry))
    }

    fn matches_entry(&self, entry: &SanEntry) -> bool {
        match (self, entry) {
            (Self::Dns(m), SanEntry::Dns(v)) => dns_matches(m, v),
            (Self::Uri(m), SanEntry::Uri(v)) => m.is_match(v),
            (Self::Email(m), SanEntry::Email(v)) => m.is_match(v),
            // A29: IP SANs match against the cert IP's canonical string form
            // (RFC 5952). `IpAddr`'s `Display` implementation already produces
            // that form (lowercase, zero-compressed IPv6).
            (Self::IpAddress(m), SanEntry::IpAddress(ip)) => m.is_match(&ip.to_string()),
            (
                Self::OtherName {
                    oid: o1,
                    matcher: m,
                },
                SanEntry::OtherName { oid: o2, value },
            ) => o1 == o2 && m.is_match(&String::from_utf8_lossy(value)),
            _ => false, // type mismatch
        }
    }
}

/// DNS SAN matching with RFC 6125 wildcard handling.
///
/// The cert SAN may contain a leftmost wildcard label (e.g., `*.example.com`).
/// Per the xDS proto documentation, wildcards are only honored when the
/// matcher type is `exact`; other matcher forms compare the SAN as a literal
/// string. The wildcard matches exactly one DNS label — `*.example.com`
/// matches `foo.example.com` but not `example.com` and not `a.b.example.com`.
fn dns_matches(matcher: &StringMatcher, cert_dns: &str) -> bool {
    if matcher.is_match(cert_dns) {
        return true;
    }
    let StringMatcher::Exact { value, ignore_case } = matcher else {
        return false;
    };
    let Some(suffix) = cert_dns.strip_prefix("*.") else {
        return false;
    };
    // `value` must be of the form `<label>.<suffix>` where <label> is
    // non-empty and contains no dots.
    let stripped = if *ignore_case {
        strip_suffix_ignore_ascii_case(value, suffix)
    } else {
        value.strip_suffix(suffix)
    };
    let Some(label) = stripped.and_then(|s| s.strip_suffix('.')) else {
        return false;
    };
    !label.is_empty() && !label.contains('.')
}

fn strip_suffix_ignore_ascii_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let split = s.len().checked_sub(suffix.len())?;
    let (head, tail) = s.split_at(split);
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::r#type::matcher::v3::StringMatcher as StringMatcherProto;
    use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;

    fn exact(v: &str) -> StringMatcherProto {
        StringMatcherProto {
            match_pattern: Some(MatchPattern::Exact(v.into())),
            ignore_case: false,
        }
    }

    fn exact_ci(v: &str) -> StringMatcherProto {
        StringMatcherProto {
            match_pattern: Some(MatchPattern::Exact(v.into())),
            ignore_case: true,
        }
    }

    fn san_proto(san_type: SanType, matcher: StringMatcherProto) -> SubjectAltNameMatcher {
        SubjectAltNameMatcher {
            san_type: san_type as i32,
            matcher: Some(matcher),
            oid: String::new(),
        }
    }

    #[test]
    fn dns_exact_match() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("api.example.com"))).unwrap();
        assert!(m.matches_any(&[SanEntry::Dns("api.example.com".into())]));
        assert!(!m.matches_any(&[SanEntry::Dns("other.example.com".into())]));
    }

    #[test]
    fn dns_wildcard_cert_matches_exact_matcher() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("foo.example.com"))).unwrap();
        // Cert carries `*.example.com`; matcher asks for `foo.example.com`.
        // RFC 6125 says wildcard matches single-label subdomains.
        assert!(m.matches_any(&[SanEntry::Dns("*.example.com".into())]));
    }

    #[test]
    fn dns_wildcard_does_not_match_bare_domain() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("example.com"))).unwrap();
        assert!(!m.matches_any(&[SanEntry::Dns("*.example.com".into())]));
    }

    #[test]
    fn dns_wildcard_does_not_match_multi_label() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("a.b.example.com"))).unwrap();
        assert!(!m.matches_any(&[SanEntry::Dns("*.example.com".into())]));
    }

    #[test]
    fn dns_wildcard_honors_ignore_case() {
        let m =
            SanMatcher::from_proto(san_proto(SanType::Dns, exact_ci("Foo.Example.Com"))).unwrap();
        assert!(m.matches_any(&[SanEntry::Dns("*.example.com".into())]));
    }

    #[test]
    fn dns_wildcard_only_for_exact_matcher() {
        // Prefix matcher must not trigger wildcard expansion — it should just
        // compare the cert DNS literally against the prefix.
        let proto = StringMatcherProto {
            match_pattern: Some(MatchPattern::Prefix("foo.".into())),
            ignore_case: false,
        };
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, proto)).unwrap();
        // Cert has `*.example.com` as literal SAN; no wildcard expansion for prefix.
        assert!(!m.matches_any(&[SanEntry::Dns("*.example.com".into())]));
        // But it should still match a literal DNS SAN with the prefix.
        assert!(m.matches_any(&[SanEntry::Dns("foo.example.com".into())]));
    }

    #[test]
    fn uri_exact_match() {
        let m = SanMatcher::from_proto(san_proto(
            SanType::Uri,
            exact("spiffe://trust/ns/prod/sa/api"),
        ))
        .unwrap();
        assert!(m.matches_any(&[SanEntry::Uri("spiffe://trust/ns/prod/sa/api".into())]));
        assert!(!m.matches_any(&[SanEntry::Uri("spiffe://trust/ns/prod/sa/other".into())]));
    }

    #[test]
    fn email_exact_match() {
        let m = SanMatcher::from_proto(san_proto(SanType::Email, exact("svc@corp.test"))).unwrap();
        assert!(m.matches_any(&[SanEntry::Email("svc@corp.test".into())]));
        assert!(!m.matches_any(&[SanEntry::Email("other@corp.test".into())]));
    }

    #[test]
    fn ip_address_canonical_match() {
        let m =
            SanMatcher::from_proto(san_proto(SanType::IpAddress, exact("2001:db8::1"))).unwrap();
        // Expanded form of the same IPv6 address.
        let canonical: IpAddr = "2001:0db8:0000:0000:0000:0000:0000:0001".parse().unwrap();
        assert!(m.matches_any(&[SanEntry::IpAddress(canonical)]));
    }

    #[test]
    fn ip_address_ipv4_match() {
        let m =
            SanMatcher::from_proto(san_proto(SanType::IpAddress, exact("192.168.1.1"))).unwrap();
        assert!(m.matches_any(&[SanEntry::IpAddress("192.168.1.1".parse().unwrap())]));
        assert!(!m.matches_any(&[SanEntry::IpAddress("192.168.1.2".parse().unwrap())]));
    }

    #[test]
    fn ip_address_prefix_match_against_canonical_form() {
        let prefix_proto = StringMatcherProto {
            match_pattern: Some(MatchPattern::Prefix("192.168.".into())),
            ignore_case: false,
        };
        let m = SanMatcher::from_proto(san_proto(SanType::IpAddress, prefix_proto)).unwrap();
        assert!(m.matches_any(&[SanEntry::IpAddress("192.168.1.5".parse().unwrap())]));
        assert!(!m.matches_any(&[SanEntry::IpAddress("10.0.0.1".parse().unwrap())]));
    }

    #[test]
    fn ip_address_ipv6_canonical_form_is_lowercased_zero_compressed() {
        // The matcher value is the canonical RFC 5952 form.
        let m =
            SanMatcher::from_proto(san_proto(SanType::IpAddress, exact("2001:db8::1"))).unwrap();
        // Various non-canonical inputs that parse to the same address must match.
        let canonical: IpAddr = "2001:0DB8:0000:0000:0000:0000:0000:0001".parse().unwrap();
        assert!(m.matches_any(&[SanEntry::IpAddress(canonical)]));
    }

    #[test]
    fn other_name_requires_oid() {
        let proto = SubjectAltNameMatcher {
            san_type: SanType::OtherName as i32,
            matcher: Some(exact("user@example.com")),
            oid: String::new(),
        };
        let err = SanMatcher::from_proto(proto).unwrap_err();
        assert!(err.to_string().contains("oid"));
    }

    #[test]
    fn other_name_matches_with_oid() {
        let proto = SubjectAltNameMatcher {
            san_type: SanType::OtherName as i32,
            matcher: Some(exact("user@example.com")),
            oid: "1.3.6.1.4.1.311.20.2.3".into(),
        };
        let m = SanMatcher::from_proto(proto).unwrap();
        assert!(m.matches_any(&[SanEntry::OtherName {
            oid: "1.3.6.1.4.1.311.20.2.3".into(),
            value: b"user@example.com".to_vec(),
        }]));
        // Different OID should not match.
        assert!(!m.matches_any(&[SanEntry::OtherName {
            oid: "1.2.3.4".into(),
            value: b"user@example.com".to_vec(),
        }]));
    }

    #[test]
    fn unspecified_san_type_is_rejected() {
        let err = SanMatcher::from_proto(san_proto(SanType::Unspecified, exact("x"))).unwrap_err();
        assert!(err.to_string().contains("UNSPECIFIED"));
    }

    #[test]
    fn missing_matcher_is_rejected() {
        let proto = SubjectAltNameMatcher {
            san_type: SanType::Dns as i32,
            matcher: None,
            oid: String::new(),
        };
        let err = SanMatcher::from_proto(proto).unwrap_err();
        assert!(err.to_string().contains("missing matcher"));
    }

    #[test]
    fn type_mismatch_does_not_match() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("api.example.com"))).unwrap();
        // Same string, but it's a URI SAN, not DNS — must not match.
        assert!(!m.matches_any(&[SanEntry::Uri("api.example.com".into())]));
    }

    #[test]
    fn matches_any_with_multiple_sans() {
        let m = SanMatcher::from_proto(san_proto(SanType::Dns, exact("api.example.com"))).unwrap();
        let sans = vec![
            SanEntry::Dns("other.example.com".into()),
            SanEntry::Uri("spiffe://foo/bar".into()),
            SanEntry::Dns("api.example.com".into()),
        ];
        assert!(m.matches_any(&sans));
    }
}
