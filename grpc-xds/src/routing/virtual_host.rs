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

//! Virtual-host domain matching for channel configuration (gRFC A27).

use std::cmp::Reverse;

use crate::resource::DomainMatcher;
use crate::resource::VirtualHost;

/// Selects the best virtual host for the channel's data-plane authority.
///
/// Matching is ASCII case-insensitive: exact > suffix > prefix > universal.
/// Longer domains win within a category; equal matches choose the first host.
/// Wildcards consume at least one character. Ports and IPv6 brackets are preserved.
// TODO: remove once the dependency manager consumes virtual-host selection.
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn find_virtual_host_index(
    authority: &str,
    virtual_hosts: &[VirtualHost],
) -> Option<usize> {
    // `DomainMatcher` stores lowercase literals, so fold the authority once.
    let authority = authority.to_ascii_lowercase();
    virtual_hosts
        .iter()
        .enumerate()
        .filter_map(|(index, host)| {
            host.domains
                .iter()
                .filter_map(|domain| domain.match_authority(&authority))
                .min()
                .map(|score| (score, index))
        })
        .min()
        .map(|(_, index)| index)
}

impl DomainMatcher {
    /// Matches an authority that has already been ASCII lowercased.
    fn match_authority(&self, authority: &str) -> Option<DomainMatchScore> {
        let (matches, score) = match self {
            Self::Exact(domain) => (authority == domain.as_str(), DomainMatchScore::Exact),
            // The wildcard must match at least one character.
            Self::Suffix(suffix) => (
                authority.len() > suffix.len() && authority.ends_with(suffix.as_str()),
                DomainMatchScore::Suffix(Reverse(suffix.len())),
            ),
            Self::Prefix(prefix) => (
                authority.len() > prefix.len() && authority.starts_with(prefix.as_str()),
                DomainMatchScore::Prefix(Reverse(prefix.len())),
            ),
            Self::Universal => (!authority.is_empty(), DomainMatchScore::Universal),
        };
        matches.then_some(score)
    }
}

/// Orders matches best-first: exact, suffix, prefix, then universal. Within
/// suffix and prefix matches, longer domains win; the stored literal is the
/// domain without its `*`, so comparing literal lengths is equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DomainMatchScore {
    Exact,
    Suffix(Reverse<usize>),
    Prefix(Reverse<usize>),
    Universal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn virtual_host(name: &str, domains: &[&str]) -> VirtualHost {
        VirtualHost {
            name: name.into(),
            domains: domains
                .iter()
                .map(|domain| DomainMatcher::try_from(*domain).unwrap())
                .collect(),
            routes: Vec::new(),
        }
    }

    #[test]
    fn domain_patterns_match_authorities() {
        for (authority, domain, expected) in [
            ("api.example.com", "api.example.com", true),
            ("API.EXAMPLE.COM", "api.example.com", true),
            ("api.example.com", "API.EXAMPLE.COM", true),
            ("api.example.com", "other.example.com", false),
            ("api.example.com", "example.com", false),
            ("api.example.com", "*.example.com", true),
            ("a.b.example.com", "*.example.com", true),
            ("a.example.com", "*.example.com", true),
            ("api.EXAMPLE.com", "*.example.COM", true),
            ("example.com", "*.example.com", false),
            (".example.com", "*.example.com", false),
            ("api.example.com.evil", "*.example.com", false),
            ("myexample.com", "*example.com", true),
            ("example.com", "*example.com", false),
            ("baz-bar.foo.com", "*-bar.foo.com", true),
            ("-bar.foo.com", "*-bar.foo.com", false),
            ("foo.example.com", "foo.*", true),
            ("FOO.example.com", "foo.*", true),
            ("foo.", "foo.*", false),
            ("foo.a", "foo.*", true),
            ("xfoo.example.com", "foo.*", false),
            ("foo-bar", "foo-*", true),
            ("foo-", "foo-*", false),
            ("foobar", "foo*", true),
            ("foo", "foo*", false),
            ("example.com", "*", true),
            ("", "*", false),
            ("api.example.com:443", "api.example.com", false),
            ("api.example.com:443", "API.EXAMPLE.COM:443", true),
            ("api.example.com:443", "*.example.com:443", true),
            ("api.example.com:443", "*.example.com", false),
            ("api.example.com:443", "*:443", true),
            ("[2001:db8::1]:443", "[2001:DB8::1]:443", true),
            ("[2001:db8::1]:443", "[2001:db8::1]", false),
            ("api.example.com.", "api.example.com", false),
            ("\u{e9}x.example.com", "*.example.com", true),
            ("\u{e9}", "a*", false),
            ("\u{e9}", "*a", false),
            ("\u{c9}.example.com", "\u{e9}.example.com", false),
        ] {
            let hosts = [virtual_host("host", &[domain])];
            assert_eq!(
                find_virtual_host_index(authority, &hosts).is_some(),
                expected,
                "authority={authority:?}, domain={domain:?}",
            );
        }
    }

    #[test]
    fn exact_match_wins_regardless_of_host_order() {
        let mut hosts = vec![
            virtual_host("universal", &["*"]),
            virtual_host("suffix", &["*.example.com"]),
            virtual_host("prefix", &["api.example.*"]),
            virtual_host("exact", &["api.example.com"]),
        ];
        for _ in 0..hosts.len() {
            let index = find_virtual_host_index("api.example.com", &hosts).unwrap();
            assert_eq!(hosts[index].name, "exact");
            hosts.rotate_left(1);
        }
    }

    #[test]
    fn suffix_beats_a_longer_prefix_regardless_of_host_order() {
        let mut hosts = vec![
            virtual_host("universal", &["*"]),
            virtual_host("prefix", &["api.example.*"]),
            virtual_host("suffix", &["*.com"]),
        ];
        for _ in 0..hosts.len() {
            let index = find_virtual_host_index("api.example.com", &hosts).unwrap();
            assert_eq!(hosts[index].name, "suffix");
            hosts.rotate_left(1);
        }
    }

    #[test]
    fn longest_domain_wins_within_a_category() {
        for domains in [["*.com", "*.example.com"], ["api.*", "api.example.*"]] {
            let mut hosts = vec![
                virtual_host("short", &[domains[0]]),
                virtual_host("long", &[domains[1]]),
            ];
            for _ in 0..hosts.len() {
                let index = find_virtual_host_index("api.example.com", &hosts).unwrap();
                assert_eq!(hosts[index].name, "long");
                hosts.reverse();
            }
        }
    }

    #[test]
    fn all_domains_are_considered_and_nonmatches_do_not_affect_precedence() {
        let hosts = vec![
            virtual_host("suffix", &["*.example.com"]),
            virtual_host("other", &["other.api.example.com", "*.other.example.com"]),
            virtual_host("exact", &["*", "other.example.com", "API.EXAMPLE.COM"]),
        ];
        assert_eq!(find_virtual_host_index("api.example.com", &hosts), Some(2));
    }

    #[test]
    fn ties_choose_the_first_virtual_host() {
        for domain in ["api.example.com", "*.example.com", "api.*", "*"] {
            let hosts = vec![
                virtual_host("first", &[domain]),
                virtual_host("second", &[&domain.to_ascii_uppercase()]),
            ];
            assert_eq!(find_virtual_host_index("api.example.com", &hosts), Some(0));
        }
    }

    #[test]
    fn no_matching_virtual_host_returns_none() {
        assert_eq!(find_virtual_host_index("api.example.com", &[]), None);
        let hosts = vec![virtual_host("other", &["other.example.com"])];
        assert_eq!(find_virtual_host_index("api.example.com", &hosts), None);
        let hosts = vec![virtual_host("universal", &["*"])];
        assert_eq!(find_virtual_host_index("", &hosts), None);
    }

    #[test]
    fn prefix_and_universal_matches_are_selected_when_more_specific_matches_fail() {
        let hosts = vec![
            virtual_host("universal", &["*"]),
            virtual_host("prefix", &["api.*"]),
            virtual_host("suffix", &["*.example.com"]),
        ];
        assert_eq!(find_virtual_host_index("api.other.com", &hosts), Some(1));
        assert_eq!(find_virtual_host_index("other.test", &hosts), Some(0));
    }

    #[test]
    fn selected_index_is_used_by_the_snapshot() {
        use std::collections::HashMap;
        use std::sync::Arc;

        use crate::resource::ListenerResource;
        use crate::resource::RouteConfigResource;
        use crate::resource::RouteSource;
        use crate::xds_config::XdsConfig;

        let route_config = Arc::new(RouteConfigResource {
            name: "routes".into(),
            virtual_hosts: vec![
                virtual_host("other", &["other.example.com"]),
                virtual_host("selected", &["api.example.com"]),
            ],
        });
        let listener = Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Inline(Arc::clone(&route_config)),
        });
        let index =
            find_virtual_host_index("api.example.com", &route_config.virtual_hosts).unwrap();
        let config = XdsConfig::try_new(listener, route_config, index, HashMap::new()).unwrap();
        assert_eq!(config.virtual_host().name, "selected");
    }
}
