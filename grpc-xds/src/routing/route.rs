/*
 *
 * Copyright 2026 gRPC authors.
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

//! Route matching for the xDS config selector (gRFC A28).

use std::borrow::Cow;

use grpc::metadata::MetadataMap;

use crate::resource::HeaderMatchSpecifier;
use crate::resource::HeaderMatcher;
use crate::resource::PathSpecifier;
use crate::resource::RouteMatch;

impl RouteMatch {
    /// Returns whether an RPC for `path` (e.g. "/pkg.Service/Method") with
    /// `metadata` matches this route.
    // TODO: remove once the resolver builds config selectors from `XdsConfig`
    // updates.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn is_match(&self, path: &str, metadata: &MetadataMap) -> bool {
        self.path_matches(path)
            && self.headers.iter().all(|h| h.is_match(metadata))
            && self
                .match_fraction
                .is_none_or(|fraction| rand::random_range(0..1_000_000) < fraction)
    }

    /// Returns whether `path` matches the path specifier. `case_sensitive`
    /// applies to prefix and exact matching only; regexes are matched as given.
    fn path_matches(&self, path: &str) -> bool {
        match &self.path_specifier {
            PathSpecifier::Prefix(prefix) if self.case_sensitive => {
                path.starts_with(prefix.as_str())
            }
            PathSpecifier::Prefix(prefix) => starts_with_ignore_ascii_case(path, prefix),
            PathSpecifier::Path(exact) if self.case_sensitive => path == exact,
            PathSpecifier::Path(exact) => path.eq_ignore_ascii_case(exact),
            PathSpecifier::SafeRegex(regex) => regex.is_match(path),
        }
    }
}

/// Returns whether `input` starts with `prefix`, ignoring ASCII case.
fn starts_with_ignore_ascii_case(input: &str, prefix: &str) -> bool {
    input
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

impl HeaderMatcher {
    /// Returns whether the header's value in `metadata` matches.
    ///
    /// `Present` and `Absent` compare the header's presence. Any other
    /// specifier fails if the header is absent, whatever `invert_match` says;
    /// otherwise it matches the value. `invert_match` then inverts the result.
    fn is_match(&self, metadata: &MetadataMap) -> bool {
        let value = header_value(metadata, &self.name);
        let matched = match (&self.match_specifier, &value) {
            (HeaderMatchSpecifier::Present, _) => value.is_some(),
            (HeaderMatchSpecifier::Absent, _) => value.is_none(),
            (_, None) => return false,
            (HeaderMatchSpecifier::Range { start, end }, Some(value)) => value
                .parse::<i64>()
                .is_ok_and(|value| value >= *start && value < *end),
            (HeaderMatchSpecifier::String(matcher), Some(value)) => matcher.is_match(value),
        };
        matched != self.invert_match
    }
}

/// Returns the value a header matcher sees for header `name`, which must be
/// lowercase (header matcher names are lowercased when RDS is parsed):
/// - `-bin` headers count as absent (gRFC A28);
/// - `content-type` is always "application/grpc";
/// - any other header is looked up in `metadata`, with multiple values joined
///   with ",".
fn header_value<'a>(metadata: &'a MetadataMap, name: &str) -> Option<Cow<'a, str>> {
    if name.ends_with("-bin") {
        return None;
    }
    if name == "content-type" {
        return Some(Cow::Borrowed("application/grpc"));
    }
    let mut values = metadata.get_all(name).iter();
    let first = values.next()?.to_str();
    let Some(second) = values.next() else {
        return Some(Cow::Borrowed(first));
    };
    let second = second.to_str();
    let mut joined = String::with_capacity(first.len() + 1 + second.len());
    joined.push_str(first);
    joined.push(',');
    joined.push_str(second);
    for value in values {
        joined.push(',');
        joined.push_str(value.to_str());
    }
    Some(Cow::Owned(joined))
}

#[cfg(test)]
mod tests {
    use grpc::metadata::MetadataValue;

    use super::*;
    use crate::resource::SafeRegex;
    use crate::resource::StringMatcher;

    const METHOD: &str = "/pkg.Service/Method";

    /// Returns a route match for `path_specifier`, with no header matchers or
    /// match fraction.
    fn path_match(path_specifier: PathSpecifier, case_sensitive: bool) -> RouteMatch {
        RouteMatch {
            path_specifier,
            headers: Vec::new(),
            case_sensitive,
            match_fraction: None,
        }
    }

    /// Returns a route match for every path, with the given header matchers.
    fn headers_match(headers: Vec<HeaderMatcher>) -> RouteMatch {
        RouteMatch {
            headers,
            ..path_match(PathSpecifier::Prefix(String::new()), true)
        }
    }

    fn header(name: &str, match_specifier: HeaderMatchSpecifier) -> HeaderMatcher {
        HeaderMatcher {
            name: name.to_string(),
            match_specifier,
            invert_match: false,
        }
    }

    fn inverted(mut matcher: HeaderMatcher) -> HeaderMatcher {
        matcher.invert_match = true;
        matcher
    }

    fn metadata(entries: &[(&'static str, &str)]) -> MetadataMap {
        let mut metadata = MetadataMap::new();
        for (key, value) in entries {
            metadata.append(*key, value.parse().unwrap());
        }
        metadata
    }

    fn regex(pattern: &str) -> SafeRegex {
        pattern.parse().unwrap()
    }

    /// Returns whether a route with only `matcher` matches `metadata`.
    fn header_matches(matcher: HeaderMatcher, metadata: &MetadataMap) -> bool {
        headers_match(vec![matcher]).is_match(METHOD, metadata)
    }

    #[test]
    fn prefix_path_matches() {
        let empty = MetadataMap::new();
        let catch_all = path_match(PathSpecifier::Prefix(String::new()), true);
        assert!(catch_all.is_match(METHOD, &empty));
        assert!(catch_all.is_match("/", &empty));

        let service = path_match(PathSpecifier::Prefix("/pkg.Service/".to_string()), true);
        assert!(service.is_match(METHOD, &empty));
        assert!(!service.is_match("/pkg.Other/Method", &empty));
        assert!(!service.is_match("/pkg.Serv", &empty));
        assert!(!service.is_match("/PKG.SERVICE/Method", &empty));

        let ignore_case = path_match(PathSpecifier::Prefix("/PKG.service/".to_string()), false);
        assert!(ignore_case.is_match(METHOD, &empty));
        assert!(!ignore_case.is_match("/pkg.Other/Method", &empty));
        // A prefix ending inside a multi-byte character doesn't panic.
        assert!(!ignore_case.is_match("/pkg.servic\u{e9}/", &empty));
    }

    #[test]
    fn exact_path_matches() {
        let empty = MetadataMap::new();
        let exact = path_match(PathSpecifier::Path(METHOD.to_string()), true);
        assert!(exact.is_match(METHOD, &empty));
        assert!(!exact.is_match("/pkg.Service/Method2", &empty));
        assert!(!exact.is_match("/pkg.service/method", &empty));

        let ignore_case = path_match(PathSpecifier::Path(METHOD.to_string()), false);
        assert!(ignore_case.is_match("/PKG.SERVICE/METHOD", &empty));
        assert!(!ignore_case.is_match("/pkg.Service/Method2", &empty));
    }

    #[test]
    fn regex_path_matches_whole_path_ignoring_case_sensitive() {
        let empty = MetadataMap::new();
        for case_sensitive in [true, false] {
            let route = path_match(
                PathSpecifier::SafeRegex(regex("/pkg\\.Service/.*")),
                case_sensitive,
            );
            assert!(route.is_match(METHOD, &empty));
            assert!(!route.is_match("/x/pkg.Service/Method", &empty));
            assert!(!route.is_match("/PKG.SERVICE/Method", &empty));
        }
    }

    #[test]
    fn string_header_matchers() {
        let md = metadata(&[("x-foo", "hello-world")]);
        for (matcher, expected) in [
            (StringMatcher::exact("hello-world", false), true),
            (StringMatcher::exact("hello", false), false),
            (StringMatcher::prefix("hello", false).unwrap(), true),
            (StringMatcher::suffix("world", false).unwrap(), true),
            (StringMatcher::contains("o-w", false).unwrap(), true),
            (StringMatcher::contains("xyz", false).unwrap(), false),
            (StringMatcher::safe_regex(regex("hello-.*")), true),
            (StringMatcher::safe_regex(regex("world")), false),
        ] {
            let description = format!("{matcher:?}");
            let matcher = header("x-foo", HeaderMatchSpecifier::String(matcher));
            assert_eq!(header_matches(matcher, &md), expected, "{description}");
        }
    }

    #[test]
    fn present_and_absent_header_matchers() {
        let md = metadata(&[("x-foo", "v")]);
        let present = || header("x-foo", HeaderMatchSpecifier::Present);
        let absent = || header("x-foo", HeaderMatchSpecifier::Absent);
        let other_present = || header("x-bar", HeaderMatchSpecifier::Present);

        assert!(header_matches(present(), &md));
        assert!(!header_matches(absent(), &md));
        assert!(!header_matches(other_present(), &md));
        assert!(!header_matches(inverted(present()), &md));
        assert!(header_matches(inverted(absent()), &md));
        assert!(header_matches(inverted(other_present()), &md));
    }

    #[test]
    fn missing_header_fails_even_when_inverted() {
        let md = MetadataMap::new();
        let exact = header(
            "x-foo",
            HeaderMatchSpecifier::String(StringMatcher::exact("v", false)),
        );
        let range = header("x-foo", HeaderMatchSpecifier::Range { start: 0, end: 10 });
        assert!(!header_matches(exact.clone(), &md));
        assert!(!header_matches(inverted(exact), &md));
        assert!(!header_matches(range.clone(), &md));
        assert!(!header_matches(inverted(range), &md));
    }

    #[test]
    fn range_header_matcher() {
        let range = || header("x-n", HeaderMatchSpecifier::Range { start: -5, end: 10 });
        for (value, expected) in [
            ("-5", true),
            ("9", true),
            ("10", false),
            ("-6", false),
            ("abc", false),
            ("1.5", false),
        ] {
            let md = metadata(&[("x-n", value)]);
            assert_eq!(header_matches(range(), &md), expected, "value: {value}");
        }
        let md = metadata(&[("x-n", "10")]);
        assert!(header_matches(inverted(range()), &md));
    }

    #[test]
    fn bin_headers_are_treated_as_absent() {
        let mut md = MetadataMap::new();
        md.insert_bin("x-trace-bin", MetadataValue::from_bytes(b"\x00\x01"));
        assert!(!header_matches(
            header("x-trace-bin", HeaderMatchSpecifier::Present),
            &md
        ));
        assert!(header_matches(
            header("x-trace-bin", HeaderMatchSpecifier::Absent),
            &md
        ));
    }

    #[test]
    fn content_type_is_application_grpc() {
        let md = metadata(&[("content-type", "text/plain")]);
        let exact = |value: &str| {
            header(
                "content-type",
                HeaderMatchSpecifier::String(StringMatcher::exact(value, false)),
            )
        };
        assert!(header_matches(exact("application/grpc"), &md));
        assert!(header_matches(
            exact("application/grpc"),
            &MetadataMap::new()
        ));
        assert!(!header_matches(exact("text/plain"), &md));
    }

    #[test]
    fn authority_header_is_absent() {
        let empty = MetadataMap::new();
        assert!(header_matches(
            header(":authority", HeaderMatchSpecifier::Absent),
            &empty
        ));
        assert!(!header_matches(
            header("host", HeaderMatchSpecifier::Present),
            &empty
        ));
        assert!(header_matches(
            header("host", HeaderMatchSpecifier::Present),
            &metadata(&[("host", "example.com")])
        ));
    }

    #[test]
    fn multiple_header_values_are_joined_with_commas() {
        let md = metadata(&[("x-foo", "a"), ("x-foo", "b"), ("x-foo", "c")]);
        let exact = |value: &str| {
            header(
                "x-foo",
                HeaderMatchSpecifier::String(StringMatcher::exact(value, false)),
            )
        };
        assert!(header_matches(exact("a,b,c"), &md));
        assert!(!header_matches(exact("a"), &md));
    }

    #[test]
    fn all_header_matchers_must_match() {
        let md = metadata(&[("x-foo", "v")]);
        let route = headers_match(vec![
            header("x-foo", HeaderMatchSpecifier::Present),
            header("x-bar", HeaderMatchSpecifier::Present),
        ]);
        assert!(!route.is_match(METHOD, &md));
    }

    #[test]
    fn fraction_zero_never_matches_and_full_always_matches() {
        let empty = MetadataMap::new();
        let with_fraction = |match_fraction| RouteMatch {
            match_fraction,
            ..path_match(PathSpecifier::Prefix(String::new()), true)
        };
        let never = with_fraction(Some(0));
        let always = with_fraction(Some(1_000_000));
        let no_fraction = with_fraction(None);
        for _ in 0..100 {
            assert!(!never.is_match(METHOD, &empty));
            assert!(always.is_match(METHOD, &empty));
            assert!(no_fraction.is_match(METHOD, &empty));
        }
    }
}
