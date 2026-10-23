/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Resource name parsing and canonicalization (gRFC A47, xRFC TP1).
//!
//! xDS resource names come in two shapes:
//!
//! - **old-style**, an opaque string such as `my-cluster`;
//! - **new-style**, a URI
//!   `xdstp://[{authority}]/{resource type}/{id}?{context parameters}`.
//!
//! Two new-style names denote the same resource if they match component-wise
//! ignoring context-parameter order, so they are canonicalized for comparison
//! and serialization. Names that do not start with `xdstp:` remain byte-for-byte
//! unchanged as old-style names; malformed names using the reserved `xdstp:`
//! scheme are rejected.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use percent_encoding::{AsciiSet, percent_decode_str, utf8_percent_encode};

use crate::error::Error;
use crate::uri::ParsedUri;

/// Scheme marking a new-style resource name.
const SCHEME_PREFIX: &str = "xdstp://";
const XDSTP_SCHEME: &str = "xdstp";

/// Escaped in the id, which is the remainder of the path and may contain `/`.
///
/// Everything outside RFC 3986 pchar: unreserved (`alnum - . _ ~`), sub-delims
/// (`! $ & ' ( ) * + , ; =`), `:`, `@`, and `/` are left as-is; non-ASCII is
/// UTF-8 percent-encoded. Shared with [`percent_encode_path`] so that encoded
/// paths and canonical resource names cannot drift apart.
///
/// [`percent_encode_path`]: crate::uri::percent_encode_path
const ID: AsciiSet = crate::uri::PATH_ENCODE_SET;

/// Escaped in the authority and resource-type components, which may not contain
/// a path separator.
///
/// Same as [`ID`] but with `/` escaped too.
const SEGMENT: AsciiSet = ID.add(b'/');

/// Escaped in context-parameter keys and values, which additionally may not
/// contain the query separators.
const PARAM: AsciiSet = SEGMENT.add(b'&').add(b'=');

/// The authority bucket identified by a resource name.
///
/// The three cases are deliberately distinct: an old-style name, a new-style
/// name with no authority, and a new-style name with an authority are three
/// different keys even when they share an id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuthorityKey {
    /// An old-style name, which is not scoped to an xDS authority.
    Legacy,
    /// A new-style name. The inner value is the decoded authority, which is the
    /// empty string for `xdstp:///...`; that remains distinct from an old-style
    /// name.
    Xdstp(Arc<str>),
}

impl AuthorityKey {
    /// The authority name, or `None` for an old-style name.
    ///
    /// Old-style names belong to no authority, so they have no name to look up
    /// — which is the distinction callers must not flatten into a map miss.
    pub fn name(&self) -> Option<&Arc<str>> {
        match self {
            Self::Legacy => None,
            Self::Xdstp(authority) => Some(authority),
        }
    }
}

/// A canonicalized xDS resource name.
///
/// Equality and hashing are over the canonical form, so two names that differ
/// only in context-parameter order — or in the percent-encoding of a component —
/// are the same key.
///
/// Identity is stored as the canonical serialized name rather than as a set of
/// decomposed fields. Equality, hashing, and serialization therefore use one
/// representation and cannot disagree about parameter ordering or encoding.
///
/// The resource type is also retained separately, allowing callers to validate
/// it against the type being watched without re-parsing the canonical wire form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceName {
    authority: AuthorityKey,
    resource_type: Option<Arc<str>>,
    canonical: Arc<str>,
}

impl ResourceName {
    /// Parses a resource name.
    ///
    /// Old-style names (anything not beginning with `xdstp:`) always
    /// succeed and are reported as [`AuthorityKey::Legacy`].
    ///
    /// A name beginning with `xdstp://` must parse as a valid new-style
    /// URI with a non-empty resource-type and id. If it does not, this
    /// returns [`Error::InvalidResourceName`], matching gRPC C++'s
    /// `InvalidArgumentError` (gRFC A47).
    ///
    /// Any other name beginning with `xdstp:` is rejected instead of being
    /// interpreted as an old-style name.
    pub fn parse(name: &str) -> Result<Self, Error> {
        match parse_xdstp(name) {
            Some(parsed) => {
                debug_assert!(parsed.canonical.starts_with(SCHEME_PREFIX));
                debug_assert!(!parsed.is_legacy());
                Ok(parsed)
            }
            None => {
                // The xdstp scheme is reserved for new-style names. Reject a
                // malformed one instead of silently routing it as legacy.
                if name.starts_with("xdstp:") {
                    Err(Error::InvalidResourceName {
                        name: name.to_string(),
                    })
                } else {
                    // An old-style name is its own canonical form, so it is
                    // stored as it arrived.
                    Ok(Self {
                        authority: AuthorityKey::Legacy,
                        resource_type: None,
                        canonical: Arc::from(name),
                    })
                }
            }
        }
    }

    /// The authority this resource belongs to.
    pub fn authority(&self) -> &AuthorityKey {
        &self.authority
    }

    /// The decoded resource type embedded in a new-style name.
    ///
    /// Legacy names do not carry a resource type and return `None`.
    pub fn resource_type(&self) -> Option<&str> {
        self.resource_type.as_deref()
    }

    /// The canonical wire form of this resource name.
    ///
    /// For new-style (`xdstp:`) names this is the canonicalized form: context
    /// parameters are sorted by key, percent-encoding is normalized, and the
    /// fragment (processing directives) is dropped. For old-style names the
    /// canonical form is byte-identical to the input.
    ///
    /// This representation is suitable for `DiscoveryRequest` resource names
    /// and cache keys.
    pub fn as_wire(&self) -> &str {
        &self.canonical
    }

    /// Whether this is an old-style name.
    pub fn is_legacy(&self) -> bool {
        matches!(self.authority, AuthorityKey::Legacy)
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical)
    }
}

impl FromStr for ResourceName {
    type Err = Error;

    /// Parses a resource name.
    ///
    /// This is the same as [`ResourceName::parse`]; implementing `FromStr`
    /// makes `"xdstp://…".parse::<ResourceName>()` work idiomatically.
    fn from_str(s: &str) -> Result<Self, Error> {
        ResourceName::parse(s)
    }
}

/// Parses a new-style name, or returns `None` if `name` is not one.
///
/// The structural split — scheme, authority, path, query, fragment — is done by
/// [`ParsedUri`]. This function then performs the xdstp-specific work:
/// splitting the path into resource-type and id, percent-decoding each component,
/// sorting context parameters, and building the canonical wire form.
fn parse_xdstp(name: &str) -> Option<ResourceName> {
    let parsed = ParsedUri::parse(name).ok()?;
    if parsed.scheme() != XDSTP_SCHEME {
        return None;
    }

    // `ParsedUri::parse` always returns `Some` for authority when it succeeds
    // (it found `://`), so this unwrap never panics. An empty authority
    // (`xdstp:///...`) is `Some("")` — a real, distinct authority.
    let authority = parsed.authority().unwrap_or("");

    // The path from `ParsedUri` starts with `/` (or is empty). Strip exactly one
    // leading `/` to get the type/id pair, matching the previous `split_once`.
    let path = parsed.path().strip_prefix('/').unwrap_or("");
    let (resource_type, id) = path.split_once('/')?;
    if resource_type.is_empty() || id.is_empty() {
        return None;
    }

    // Fragment (processing directives) is already split off by `ParsedUri` and
    // dropped here: it selects how a resource is delivered, not which resource
    // it is, so it is not part of the identity.
    let authority = decode(authority)?;
    let resource_type = decode(resource_type)?;
    let id = decode(id)?;
    let params = parse_context_params(parsed.query())?;

    let mut canonical = String::with_capacity(name.len());
    canonical.push_str(SCHEME_PREFIX);
    canonical.extend(utf8_percent_encode(&authority, &SEGMENT));
    canonical.push('/');
    canonical.extend(utf8_percent_encode(&resource_type, &SEGMENT));
    canonical.push('/');
    canonical.extend(utf8_percent_encode(&id, &ID));
    for (i, (key, value)) in params.iter().enumerate() {
        canonical.push(if i == 0 { '?' } else { '&' });
        canonical.extend(utf8_percent_encode(key, &PARAM));
        canonical.push('=');
        canonical.extend(utf8_percent_encode(value, &PARAM));
    }

    Some(ResourceName {
        authority: AuthorityKey::Xdstp(Arc::from(&*authority)),
        resource_type: Some(Arc::from(&*resource_type)),
        canonical: Arc::from(canonical),
    })
}

/// Decodes and canonically orders the context parameters of a new-style name.
///
/// Keys are sorted so that names differing only in parameter order compare
/// equal. A repeated key keeps its first value: the specification leaves this
/// undefined, so first-wins is chosen here and applied consistently.
fn parse_context_params(query: Option<&str>) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
    let Some(query) = query else {
        return Some(Vec::new());
    };

    let mut params: Vec<(Cow<'_, str>, Cow<'_, str>)> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key)?;
        // A linear scan is sufficient because a name carries only a handful of
        // parameters. Revisit this if that assumption stops being true.
        if params.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        params.push((key, decode(value)?));
    }
    params.sort_by(|(a, _), (b, _)| a.cmp(b));
    Some(params)
}

/// Percent-decodes a component, or `None` if the result is not UTF-8.
///
/// Lossy decoding would be wrong here rather than merely imprecise: every
/// invalid byte collapses to U+FFFD, so `%FF` and `%FE` produce the same
/// canonical name and therefore the same cache key for two different resources.
/// Returning `None` causes [`ResourceName::parse`] to reject a name using the
/// reserved `xdstp:` scheme.
fn decode(component: &str) -> Option<Cow<'_, str>> {
    percent_decode_str(component).decode_utf8().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xdstp(authority: &str) -> AuthorityKey {
        AuthorityKey::Xdstp(Arc::from(authority))
    }

    #[test]
    fn test_parse() {
        // (input, expected authority, expected canonical form)
        let cases = [
            ("old-style-name", AuthorityKey::Legacy, "old-style-name"),
            ("", AuthorityKey::Legacy, ""),
            // Not a URI at all: no `//` after the scheme.
            ("a:/b/c", AuthorityKey::Legacy, "a:/b/c"),
            // A scheme we do not recognize is an opaque old-style name.
            (
                "https://auth/type/id",
                AuthorityKey::Legacy,
                "https://auth/type/id",
            ),
            ("xdstp:///type/id", xdstp(""), "xdstp:///type/id"),
            (
                "xdstp://auth/type/id",
                xdstp("auth"),
                "xdstp://auth/type/id",
            ),
            // The id is the whole remaining path, separators included.
            (
                "xdstp://auth/type/a/b/c",
                xdstp("auth"),
                "xdstp://auth/type/a/b/c",
            ),
            (
                "xdstp://auth/type/id?a=1&b=2",
                xdstp("auth"),
                "xdstp://auth/type/id?a=1&b=2",
            ),
            // Context parameters are sorted by key.
            (
                "xdstp://auth/type/id?b=2&a=1",
                xdstp("auth"),
                "xdstp://auth/type/id?a=1&b=2",
            ),
            // A repeated key keeps its first value.
            (
                "xdstp://auth/type/id?a=1&a=2",
                xdstp("auth"),
                "xdstp://auth/type/id?a=1",
            ),
            // Percent-encoding is normalized away where it is not required.
            (
                "xdstp://auth/type/%41",
                xdstp("auth"),
                "xdstp://auth/type/A",
            ),
            // ...and re-applied where it is.
            (
                "xdstp://auth/type/a%3Fb",
                xdstp("auth"),
                "xdstp://auth/type/a%3Fb",
            ),
            // Processing directives are not part of a resource's identity.
            (
                "xdstp://auth/type/id#entry=foo",
                xdstp("auth"),
                "xdstp://auth/type/id",
            ),
        ];

        for (input, want_authority, want_canonical) in cases {
            let got = ResourceName::parse(input).expect("should parse");
            assert_eq!(*got.authority(), want_authority, "authority of {input:?}");
            assert_eq!(got.as_wire(), want_canonical, "canonical form of {input:?}");
        }
    }

    /// Names beginning with `xdstp:` that fail structural validation
    /// must return an error, not fall back to Legacy.
    /// Invalid UTF-8 is rejected rather than decoded lossily, because lossy
    /// decoding could collapse distinct resource names onto the same key.
    #[test]
    fn test_malformed_xdstp_names_are_rejected() {
        let bad = [
            "xdstp:not-a-uri",       // reserved scheme without URI authority form
            "xdstp://auth/id",       // missing resource-type segment
            "xdstp://auth/type/",    // empty id
            "xdstp://auth",          // no path at all
            "xdstp://",              // only scheme
            "xdstp://auth/type/%FF", // non-UTF-8 after percent decode
            "xdstp://auth/type/%FE", // non-UTF-8 after percent decode
        ];
        for input in bad {
            assert!(
                ResourceName::parse(input).is_err(),
                "{input:?} should be rejected as InvalidResourceName"
            );
        }
    }

    #[test]
    fn test_old_style_names_round_trip_unchanged() {
        // Non-federated clients must keep behaving exactly as they do today, so
        // an old-style name has to survive parsing byte-for-byte — including
        // characters that would be rewritten in a new-style name.
        for name in ["my-cluster", "my cluster%2F?#"] {
            let parsed = ResourceName::parse(name).expect("old-style");
            assert!(parsed.is_legacy(), "{name:?} should be old-style");
            assert_eq!(parsed.as_wire(), name);
        }
    }

    /// The three buckets A47 requires are three buckets: an old-style name, an
    /// empty-authority name, and a named-authority name never collide, even when
    /// they carry the same id.
    #[test]
    fn test_the_three_authority_buckets_are_distinct() {
        let legacy = ResourceName::parse("cluster-a").unwrap();
        let unnamed =
            ResourceName::parse("xdstp:///envoy.config.cluster.v3.Cluster/cluster-a").unwrap();
        let named =
            ResourceName::parse("xdstp://auth/envoy.config.cluster.v3.Cluster/cluster-a").unwrap();

        assert_eq!(*legacy.authority(), AuthorityKey::Legacy);
        assert_eq!(*unnamed.authority(), xdstp(""));
        assert_eq!(*named.authority(), xdstp("auth"));
        assert_eq!(legacy.resource_type(), None);
        assert_eq!(
            unnamed.resource_type(),
            Some("envoy.config.cluster.v3.Cluster")
        );

        assert_ne!(legacy, unnamed);
        assert_ne!(unnamed, named);
        assert_ne!(legacy, named);
    }

    #[test]
    fn test_context_param_order_does_not_affect_identity() {
        let a = ResourceName::parse("xdstp://auth/type/id?a=1&b=2&c=3").unwrap();
        let b = ResourceName::parse("xdstp://auth/type/id?c=3&a=1&b=2").unwrap();

        assert_eq!(a, b);
        assert_eq!(a.as_wire(), b.as_wire());
    }

    #[test]
    fn test_canonical_form_is_stable_under_reparsing() {
        // Parsing the canonical serialized form again must be a no-op.
        for name in [
            "cluster-a",
            "xdstp:///type/id",
            "xdstp://auth/type/id?b=2&a=1",
            "xdstp://auth/type/a%3Fb?k=%26v",
            "xdstp://auth/type/id#entry=foo",
        ] {
            let once = ResourceName::parse(name).unwrap();
            let twice = ResourceName::parse(once.as_wire()).unwrap();
            assert_eq!(once, twice, "reparsing {name:?} changed it");
        }
    }

    #[test]
    fn test_separators_in_components_survive_a_round_trip() {
        let parsed = ResourceName::parse("xdstp://auth/type/id?k=a%26b%3Dc").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://auth/type/id?k=a%26b%3Dc");

        // Re-parsing must not read the escaped `&` as a parameter separator.
        let reparsed = ResourceName::parse(parsed.as_wire()).unwrap();
        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn test_id_is_opaque() {
        // The id is whatever the control plane says it is, so path-like sequences in it
        // carry no meaning here. A WHATWG-conformant URL parser resolves them:
        // `url::Url::parse("xdstp://auth/type/../escaped")` serializes back as
        // `xdstp://auth/escaped`, which names a different resource and, in the worst
        // case, one belonging to a different authority. `ParsedUri` (the RFC 3986
        // splitter in `crate::uri`) preserves them verbatim, and so do we.
        for name in [
            "xdstp://auth/type/../escaped",
            "xdstp://auth/type/a/./b",
            "xdstp://auth/type/..",
            "xdstp://auth/type/../../..",
        ] {
            let parsed = ResourceName::parse(name).unwrap();
            assert_eq!(parsed.as_wire(), name, "{name:?} was rewritten");
            assert_eq!(
                *parsed.authority(),
                xdstp("auth"),
                "{name:?} changed authority"
            );
        }
    }

    /// This pins the limit of the opacity guaranteed by `test_id_is_opaque`: an
    /// escaped separator in the id is *not* preserved.
    ///
    /// This follows from decoding and re-encoding. `/` cannot be in the escape set
    /// for the id — the id is the whole remaining path and multi-segment ids are
    /// legal — so a decoded `%2F` re-encodes as a plain `/`, and two names RFC 3986
    /// would keep apart become one cache key. Preserving the distinction means not
    /// decoding the id at all, which is Java's choice; Go decodes, as here. The
    /// specification does not settle it.
    #[test]
    fn test_escaped_separator_in_the_id_is_not_preserved() {
        let escaped = ResourceName::parse("xdstp://auth/type/my%2Fservice").unwrap();
        assert_eq!(escaped.as_wire(), "xdstp://auth/type/my/service");
        assert_eq!(
            escaped,
            ResourceName::parse("xdstp://auth/type/my/service").unwrap()
        );
    }

    #[test]
    fn test_valueless_context_param() {
        let parsed = ResourceName::parse("xdstp://auth/type/id?flag").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://auth/type/id?flag=");
        assert_eq!(ResourceName::parse(parsed.as_wire()).unwrap(), parsed);
    }

    /// Non-ASCII in the id must be UTF-8 percent-encoded on the wire.
    /// Escapes everything outside RFC 3986 pchar.
    #[test]
    fn test_non_ascii_in_id_is_percent_encoded() {
        let parsed = ResourceName::parse("xdstp://a/t/café").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://a/t/caf%C3%A9");
    }

    /// Reserved characters `[` and `]` in the id must be percent-encoded
    /// on the wire.
    #[test]
    fn test_brackets_in_id_are_escaped() {
        let parsed = ResourceName::parse("xdstp://auth/type/[v1]svc").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://auth/type/%5Bv1%5Dsvc");
    }

    /// Sub-delims and pchar extras (`! $ & ' ( ) * + , ; = : @`) are
    /// unescaped in the id, per RFC 3986 pchar.
    #[test]
    fn test_sub_delims_unescaped_in_id() {
        let parsed = ResourceName::parse("xdstp://auth/type/a!b$c").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://auth/type/a!b$c");
    }

    /// Percent-encoded authority is decoded on parse, then re-encoded on
    /// the wire. `xdstp://auth%2Fzone/t/id` decodes authority to `auth/zone`,
    /// and the canonical form re-encodes the `/` as `%2F` (since SEGMENT
    /// escapes `/`). Round-trip must be stable.
    #[test]
    fn test_encoded_authority_round_trips() {
        let name = "xdstp://auth%2Fzone/test.Fake/resource-a";
        let parsed = ResourceName::parse(name).unwrap();
        // The decoded authority is `auth/zone`.
        assert_eq!(
            parsed.authority().name().map(|a| a.as_ref()),
            Some("auth/zone")
        );
        // The wire form re-encodes the `/` in the authority as `%2F`.
        assert_eq!(parsed.as_wire(), name);
        // Round-trip: parsing the wire form yields the same name.
        let reparsed = ResourceName::parse(parsed.as_wire()).unwrap();
        assert_eq!(reparsed.as_wire(), parsed.as_wire());
    }

    /// The structural URI parser must not consume an escaping layer before
    /// resource-name parsing does. These are two distinct authority names.
    #[test]
    fn test_authority_is_decoded_exactly_once() {
        let encoded_slash = ResourceName::parse("xdstp://auth%2Fzone/t/id").unwrap();
        let encoded_percent = ResourceName::parse("xdstp://auth%252Fzone/t/id").unwrap();

        assert_eq!(
            encoded_slash.authority().name().map(|a| a.as_ref()),
            Some("auth/zone")
        );
        assert_eq!(
            encoded_percent.authority().name().map(|a| a.as_ref()),
            Some("auth%2Fzone")
        );
        assert_ne!(encoded_slash.authority(), encoded_percent.authority());
        assert_eq!(encoded_percent.as_wire(), "xdstp://auth%252Fzone/t/id");
    }

    /// Reserved characters in the authority (e.g. `[`, `]`, space) are
    /// percent-encoded on the wire.
    #[test]
    fn test_reserved_chars_in_authority_encoded() {
        let parsed = ResourceName::parse("xdstp://auth zone/t/id").unwrap();
        assert_eq!(parsed.as_wire(), "xdstp://auth%20zone/t/id");
    }
}
