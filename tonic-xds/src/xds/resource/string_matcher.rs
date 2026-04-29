//! Generic string matcher for xDS configuration.
//!
//! Mirrors [`envoy.type.matcher.v3.StringMatcher`]: a small set of string
//! comparison modes (`exact` / `prefix` / `suffix` / `contains` / `safe_regex`)
//! with optional ASCII case-insensitive matching on the string-literal variants.
//!
//! Used wherever an xDS config carries a `StringMatcher` — HTTP header matching
//! (gRFC A28) and SAN matching for server authorization (gRFC A29).

use envoy_types::pb::envoy::r#type::matcher::v3::StringMatcher as StringMatcherProto;
use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;
use regex::Regex;
use xds_client::Error;

/// Validated [`envoy.type.matcher.v3.StringMatcher`].
#[derive(Debug, Clone)]
pub(crate) enum StringMatcher {
    Exact { value: String, ignore_case: bool },
    Prefix { value: String, ignore_case: bool },
    Suffix { value: String, ignore_case: bool },
    Contains { value: String, ignore_case: bool },
    SafeRegex(Regex),
}

impl StringMatcher {
    /// Parse and validate an Envoy `StringMatcher` proto.
    ///
    /// Returns an error if the `match_pattern` oneof is unset or carries an
    /// unsupported variant, or if a `safe_regex` fails to compile.
    pub(crate) fn from_proto(proto: StringMatcherProto) -> xds_client::Result<Self> {
        let ignore_case = proto.ignore_case;
        match proto.match_pattern {
            Some(MatchPattern::Exact(value)) => Ok(Self::Exact { value, ignore_case }),
            Some(MatchPattern::Prefix(value)) => Ok(Self::Prefix { value, ignore_case }),
            Some(MatchPattern::Suffix(value)) => Ok(Self::Suffix { value, ignore_case }),
            Some(MatchPattern::Contains(value)) => Ok(Self::Contains { value, ignore_case }),
            Some(MatchPattern::SafeRegex(r)) => {
                let re = Regex::new(&r.regex)
                    .map_err(|e| Error::Validation(format!("invalid regex '{}': {e}", r.regex)))?;
                Ok(Self::SafeRegex(re))
            }
            None => Err(Error::Validation(
                "StringMatcher has no match_pattern set".into(),
            )),
            _ => Err(Error::Validation(
                "unsupported StringMatcher pattern".into(),
            )),
        }
    }

    /// Evaluate the matcher against a value.
    pub(crate) fn is_match(&self, v: &str) -> bool {
        match self {
            Self::Exact { value, ignore_case } => {
                if *ignore_case {
                    v.eq_ignore_ascii_case(value)
                } else {
                    v == value
                }
            }
            Self::Prefix { value, ignore_case } => {
                if *ignore_case {
                    v.to_ascii_lowercase()
                        .starts_with(&value.to_ascii_lowercase())
                } else {
                    v.starts_with(value.as_str())
                }
            }
            Self::Suffix { value, ignore_case } => {
                if *ignore_case {
                    v.to_ascii_lowercase()
                        .ends_with(&value.to_ascii_lowercase())
                } else {
                    v.ends_with(value.as_str())
                }
            }
            Self::Contains { value, ignore_case } => {
                if *ignore_case {
                    v.to_ascii_lowercase().contains(&value.to_ascii_lowercase())
                } else {
                    v.contains(value.as_str())
                }
            }
            Self::SafeRegex(re) => re.is_match(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::r#type::matcher::v3::RegexMatcher;

    fn proto(pattern: MatchPattern, ignore_case: bool) -> StringMatcherProto {
        StringMatcherProto {
            match_pattern: Some(pattern),
            ignore_case,
        }
    }

    #[test]
    fn exact_case_sensitive() {
        let m = StringMatcher::from_proto(proto(MatchPattern::Exact("Foo".into()), false)).unwrap();
        assert!(m.is_match("Foo"));
        assert!(!m.is_match("foo"));
        assert!(!m.is_match("Food"));
    }

    #[test]
    fn exact_ignore_case() {
        let m = StringMatcher::from_proto(proto(MatchPattern::Exact("Foo".into()), true)).unwrap();
        assert!(m.is_match("Foo"));
        assert!(m.is_match("foo"));
        assert!(m.is_match("FOO"));
        assert!(!m.is_match("Food"));
    }

    #[test]
    fn prefix() {
        let m =
            StringMatcher::from_proto(proto(MatchPattern::Prefix("abc".into()), false)).unwrap();
        assert!(m.is_match("abcdef"));
        assert!(m.is_match("abc"));
        assert!(!m.is_match("ab"));
        assert!(!m.is_match("xabc"));
    }

    #[test]
    fn prefix_ignore_case() {
        let m = StringMatcher::from_proto(proto(MatchPattern::Prefix("ABC".into()), true)).unwrap();
        assert!(m.is_match("abcdef"));
        assert!(m.is_match("ABCdef"));
        assert!(!m.is_match("xabc"));
    }

    #[test]
    fn suffix() {
        let m =
            StringMatcher::from_proto(proto(MatchPattern::Suffix("xyz".into()), false)).unwrap();
        assert!(m.is_match("abcxyz"));
        assert!(m.is_match("xyz"));
        assert!(!m.is_match("xy"));
        assert!(!m.is_match("xyzz"));
    }

    #[test]
    fn suffix_ignore_case() {
        let m = StringMatcher::from_proto(proto(MatchPattern::Suffix("XYZ".into()), true)).unwrap();
        assert!(m.is_match("abcxyz"));
        assert!(m.is_match("abcXYZ"));
    }

    #[test]
    fn contains() {
        let m =
            StringMatcher::from_proto(proto(MatchPattern::Contains("mid".into()), false)).unwrap();
        assert!(m.is_match("amidz"));
        assert!(m.is_match("mid"));
        assert!(!m.is_match("MID"));
    }

    #[test]
    fn contains_ignore_case() {
        let m =
            StringMatcher::from_proto(proto(MatchPattern::Contains("MID".into()), true)).unwrap();
        assert!(m.is_match("amidz"));
        assert!(m.is_match("aMIDz"));
    }

    #[test]
    fn safe_regex() {
        let m = StringMatcher::from_proto(proto(
            MatchPattern::SafeRegex(RegexMatcher {
                regex: r"^foo\d+$".into(),
                ..Default::default()
            }),
            false,
        ))
        .unwrap();
        assert!(m.is_match("foo123"));
        assert!(!m.is_match("foo"));
        assert!(!m.is_match("xfoo123"));
    }

    #[test]
    fn safe_regex_invalid_is_rejected() {
        let err = StringMatcher::from_proto(proto(
            MatchPattern::SafeRegex(RegexMatcher {
                regex: "(unclosed".into(),
                ..Default::default()
            }),
            false,
        ))
        .unwrap_err();
        assert!(err.to_string().contains("invalid regex"));
    }

    #[test]
    fn missing_match_pattern_is_rejected() {
        let p = StringMatcherProto {
            match_pattern: None,
            ignore_case: false,
        };
        let err = StringMatcher::from_proto(p).unwrap_err();
        assert!(err.to_string().contains("no match_pattern set"));
    }
}
