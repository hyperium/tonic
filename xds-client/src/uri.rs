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

//! Minimal RFC 3986 URI splitting — no normalization.
//!
//! The Rust `url` crate is WHATWG-based: it resolves dot segments during parse,
//! applies IDNA/punycode to hosts, and re-encodes paths. Those transformations
//! are not expected for xDS, where both the authority and the path are opaque
//! strings that must survive a round trip byte-for-byte (gRFC A47).
//!
//! This module provides a purely structural split:
//! `scheme://authority/path?query#fragment`, with no transformation of any
//! component. Callers that need percent-decoding or canonicalization apply it
//! themselves, see [`crate::resource::name`] for the `xdstp:` resource-name layer.

use std::fmt;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Error returned by [`ParsedUri::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriParseError {
    /// The input does not contain `://`, so it has no parseable scheme.
    NoScheme,
    /// The scheme is empty.
    EmptyScheme,
}

impl fmt::Display for UriParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoScheme => f.write_str("URI has no scheme (missing '://')"),
            Self::EmptyScheme => f.write_str("URI scheme is empty"),
        }
    }
}

impl std::error::Error for UriParseError {}

/// A URI split into its RFC 3986 components, borrowed from the input.
///
/// No component is normalized, decoded, or re-encoded. Dot segments in
/// [`path`](Self::path) are preserved. The authority is not punycoded.
/// The fragment is retained (unlike `http::Uri`, which drops it on display).
///
/// # Example
///
/// ```
/// use xds_client::uri::ParsedUri;
///
/// let uri = ParsedUri::parse("xds://authority/my-service").unwrap();
/// assert_eq!(uri.scheme(), "xds");
/// assert_eq!(uri.authority(), Some("authority"));
/// assert_eq!(uri.path(), "/my-service");
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedUri<'a> {
    scheme: &'a str,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl<'a> ParsedUri<'a> {
    /// Splits a URI string into its components.
    ///
    /// Returns [`UriParseError::NoScheme`] if the input does not contain `://`.
    /// An empty authority (`xds:///path`) is valid and reported as
    /// [`authority`](Self::authority) `= Some("")`.
    ///
    /// This function does **not** validate the scheme characters, normalize
    /// the path, or decode any component. It is a pure structural split.
    pub fn parse(input: &'a str) -> Result<Self, UriParseError> {
        let (scheme, rest) = input.split_once("://").ok_or(UriParseError::NoScheme)?;

        if scheme.is_empty() {
            return Err(UriParseError::EmptyScheme);
        }

        // Split off the fragment first so it doesn't interfere with query/path.
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, Some(f)),
            None => (rest, None),
        };

        // Split off the query.
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };

        // The authority is everything up to the first `/`. The path is the
        // remainder, *including* the leading `/` — matching Go's `net/url`
        // and Java's `java.net.URI`.
        //
        // `rest.find('/')` gives us the byte index of the first `/`, so
        // `rest[..idx]` is the authority and `rest[idx..]` is the path
        // (starting with `/`). If there is no `/`, the authority is the
        // whole `rest` and the path is empty.
        let (authority, path) = match rest.find('/') {
            Some(idx) => (Some(&rest[..idx]), &rest[idx..]),
            None => (Some(rest), ""),
        };

        Ok(Self {
            scheme,
            authority,
            path,
            query,
            fragment,
        })
    }

    /// The scheme, e.g. `xds` or `xdstp`.
    pub fn scheme(&self) -> &'a str {
        self.scheme
    }

    /// The authority component.
    ///
    /// Successful parsing always finds a `://` delimiter and therefore returns
    /// `Some`. For `xds:///service`, the authority is `Some("")`.
    pub fn authority(&self) -> Option<&str> {
        self.authority
    }

    /// The path, including the leading `/`.
    ///
    /// Dot segments are preserved — `/a/../b` stays `/a/../b`.
    pub fn path(&self) -> &'a str {
        self.path
    }

    /// The query string without the leading `?`, or `None`.
    pub fn query(&self) -> Option<&'a str> {
        self.query
    }

    /// The fragment without the leading `#`, or `None`.
    pub fn fragment(&self) -> Option<&'a str> {
        self.fragment
    }
}

/// Characters that must be percent-encoded in a URI path.
///
/// Matches the complement of C++'s `IsPathChar`: everything except
/// unreserved (`alnum - . _ ~`), sub-delims (`! $ & ' ( ) * + , ; =`),
/// `:`, `@`, and `/`. Non-ASCII is escaped (UTF-8 percent-encoded).
///
/// `%` IS in the set, so existing `%XX` sequences are double-escaped.
/// Callers that need to preserve existing encoding must percent-decode
/// first.
pub(crate) const PATH_ENCODE_SET: AsciiSet = NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~') // unreserved
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=') // sub-delims
    .remove(b':')
    .remove(b'@') // pchar extras
    .remove(b'/'); // path separator

const AUTHORITY_ENCODE_SET: AsciiSet = PATH_ENCODE_SET.add(b'/');

/// Percent-encodes a string for use as a URI path.
///
/// `%` IS escaped, so callers must percent-decode the input first if it
/// may contain existing `%XX` sequences. Non-ASCII characters are
/// UTF-8 percent-encoded. Escapes everything outside RFC 3986 pchar plus `/`:
/// unreserved, sub-delims, `:`, `@`, and `/` remain unescaped.
pub fn percent_encode_path(input: &str) -> String {
    utf8_percent_encode(input, &PATH_ENCODE_SET).to_string()
}

/// Percent-encodes a decoded xDS authority for use in a URI.
///
/// Authorities are opaque xDS identifiers, so this uses the path rules above
/// but additionally escapes `/`, preventing it from being mistaken for the
/// start of the URI path.
pub fn percent_encode_authority(input: &str) -> String {
    utf8_percent_encode(input, &AUTHORITY_ENCODE_SET).to_string()
}

/// Percent-decodes a URI path component.
///
/// Returns the decoded string, or the input as-is if decoding fails
/// (e.g. invalid UTF-8 after decoding). This is the decode counterpart
/// of [`percent_encode_path`], used by callers that need to decode before
/// re-encoding.
pub fn percent_decode_path(input: &str) -> std::borrow::Cow<'_, str> {
    percent_encoding::percent_decode_str(input)
        .decode_utf8()
        .unwrap_or(std::borrow::Cow::Borrowed(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> ParsedUri<'_> {
        ParsedUri::parse(input).unwrap_or_else(|e| panic!("parsing {input:?}: {e}"))
    }

    #[test]
    fn test_basic_split() {
        let u = parse("xds://auth/svc");
        assert_eq!(u.scheme(), "xds");
        assert_eq!(u.authority(), Some("auth"));
        assert_eq!(u.path(), "/svc");
        assert_eq!(u.query(), None);
        assert_eq!(u.fragment(), None);
    }

    #[test]
    fn test_empty_authority() {
        let u = parse("xds:///my-service");
        assert_eq!(u.scheme(), "xds");
        assert_eq!(u.authority(), Some(""));
        assert_eq!(u.path(), "/my-service");
    }

    #[test]
    fn test_no_path() {
        let u = parse("xds://auth");
        assert_eq!(u.scheme(), "xds");
        assert_eq!(u.authority(), Some("auth"));
        assert_eq!(u.path(), "");
    }

    #[test]
    fn test_query_and_fragment() {
        let u = parse("xdstp://auth/type/id?b=2&a=1#frag");
        assert_eq!(u.scheme(), "xdstp");
        assert_eq!(u.authority(), Some("auth"));
        assert_eq!(u.path(), "/type/id");
        assert_eq!(u.query(), Some("b=2&a=1"));
        assert_eq!(u.fragment(), Some("frag"));
    }

    #[test]
    fn test_dot_segments_preserved() {
        // The whole point: WHATWG resolves these, we don't.
        let u = parse("xdstp://auth/type/../escaped");
        assert_eq!(u.path(), "/type/../escaped");

        let u = parse("xds:///svc/../prod");
        assert_eq!(u.path(), "/svc/../prod");
    }

    #[test]
    fn test_percent_encoding_preserved() {
        let u = parse("xds:///my%20service");
        assert_eq!(u.path(), "/my%20service");

        let u = parse("xds://auth%252Fzone/service");
        assert_eq!(u.authority(), Some("auth%252Fzone"));
    }

    #[test]
    fn test_unicode_host_preserved() {
        // No IDNA/punycode — the raw bytes are returned as-is.
        let u = parse("xds://münchäen/svc");
        assert_eq!(u.authority(), Some("münchäen"));
    }

    #[test]
    fn test_fragment_only() {
        let u = parse("xdstp://auth/type/id#entry=foo");
        assert_eq!(u.path(), "/type/id");
        assert_eq!(u.query(), None);
        assert_eq!(u.fragment(), Some("entry=foo"));
    }

    #[test]
    fn test_no_scheme_returns_error() {
        assert_eq!(ParsedUri::parse("not-a-uri"), Err(UriParseError::NoScheme));
    }

    #[test]
    fn test_empty_scheme_returns_error() {
        assert_eq!(
            ParsedUri::parse(":///path"),
            Err(UriParseError::EmptyScheme)
        );
    }

    #[test]
    fn test_multi_segment_path() {
        let u = parse("xdstp://auth/envoy.config.listener.v3.Listener/svc");
        assert_eq!(u.path(), "/envoy.config.listener.v3.Listener/svc");
    }

    #[test]
    fn test_root_path() {
        let u = parse("xds:///");
        assert_eq!(u.authority(), Some(""));
        assert_eq!(u.path(), "/");
    }

    #[test]
    fn test_authority_with_port() {
        let u = parse("xds://auth:8080/svc");
        assert_eq!(u.authority(), Some("auth:8080"));
        assert_eq!(u.path(), "/svc");
    }
}
