//! Per-request route matching on validated resource types.
//!
//! Operates directly on [`RouteConfigResource`] and its sub-types.
//! The matching pipeline: domain → path → headers.
//!
//! Domain matching follows gRFC A27 priority:
//! 1. Exact match
//! 2. Suffix wildcard (`*.foo.com`)
//! 3. Prefix wildcard (`foo.*`)
//! 4. Universal wildcard `*`
//!
//! Within each category, the most specific (longest non-wildcard part) wins.

use std::cmp::Reverse;

use crate::xds::resource::route_config::{
    HeaderMatchSpecifierConfig, HeaderMatcherConfig, PathSpecifierConfig, RouteConfig,
    RouteConfigAction, RouteConfigMatch, RouteConfigResource, VirtualHostConfig,
};

/// Error returned when route matching fails.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum RoutingError {
    #[error("no matching virtual host for authority '{0}'")]
    NoMatchingVirtualHost(String),
    #[error("no matching route in virtual host for path '{0}'")]
    NoMatchingRoute(String),
}

impl RouteConfigResource {
    /// Match a request and return the target cluster action.
    ///
    /// Performs domain matching on the authority, then walks routes in order
    /// to find the first match.
    pub(crate) fn route(
        &self,
        authority: &str,
        path: &str,
        headers: &http::HeaderMap,
    ) -> Result<&RouteConfigAction, RoutingError> {
        let vh = find_best_matching_virtual_host(authority, &self.virtual_hosts)
            .ok_or_else(|| RoutingError::NoMatchingVirtualHost(authority.to_string()))?;

        for route in &vh.routes {
            if route_matches(route, path, headers) {
                return Ok(&route.action);
            }
        }

        Err(RoutingError::NoMatchingRoute(path.to_string()))
    }
}

const WILDCARD: &str = "*";

/// Finds the best-matching virtual host for the given authority.
fn find_best_matching_virtual_host<'a>(
    authority: &str,
    virtual_hosts: &'a [VirtualHostConfig],
) -> Option<&'a VirtualHostConfig> {
    virtual_hosts
        .iter()
        .filter_map(|vh| {
            let best_score = vh
                .domains
                .iter()
                .filter_map(|d| match_domain(authority, d))
                .min()?;
            Some((best_score, vh))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, vh)| vh)
}

/// How well a domain pattern matched an authority.
///
/// Sorts naturally so that better matches are smaller:
/// match type (Exact < Suffix < Prefix < Universal), then higher
/// specificity (more non-wildcard characters) breaks ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DomainMatchScore(DomainMatchType, Reverse<usize>);

/// Domain match types ordered by priority (lower is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DomainMatchType {
    Exact = 0,
    Suffix = 1,
    Prefix = 2,
    Universal = 3,
}

fn match_domain(authority: &str, pattern: &str) -> Option<DomainMatchScore> {
    if pattern == WILDCARD {
        return Some(DomainMatchScore(DomainMatchType::Universal, Reverse(0)));
    }

    let authority_lower = authority.to_ascii_lowercase();
    let pattern_lower = pattern.to_ascii_lowercase();

    if authority_lower == pattern_lower {
        return Some(DomainMatchScore(
            DomainMatchType::Exact,
            Reverse(pattern.len()),
        ));
    }

    if let Some(suffix) = pattern_lower.strip_prefix(WILDCARD)
        && authority_lower.ends_with(suffix)
        && authority_lower.len() > suffix.len()
    {
        return Some(DomainMatchScore(
            DomainMatchType::Suffix,
            Reverse(suffix.len()),
        ));
    }

    if let Some(prefix) = pattern_lower.strip_suffix(WILDCARD)
        && authority_lower.starts_with(prefix)
        && authority_lower.len() > prefix.len()
    {
        return Some(DomainMatchScore(
            DomainMatchType::Prefix,
            Reverse(prefix.len()),
        ));
    }

    None
}

fn route_matches(route: &RouteConfig, path: &str, headers: &http::HeaderMap) -> bool {
    match_path(&route.match_criteria, path) && match_headers(&route.match_criteria, headers)
}

fn match_path(criteria: &RouteConfigMatch, path: &str) -> bool {
    match &criteria.path_specifier {
        PathSpecifierConfig::Prefix(prefix) => {
            if prefix.is_empty() {
                return true;
            }
            if criteria.case_sensitive {
                path.starts_with(prefix.as_str())
            } else {
                path.to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
            }
        }
        PathSpecifierConfig::Path(exact) => {
            if criteria.case_sensitive {
                path == exact
            } else {
                path.eq_ignore_ascii_case(exact)
            }
        }
        PathSpecifierConfig::SafeRegex(re) => re.is_match(path),
    }
}

fn match_headers(criteria: &RouteConfigMatch, headers: &http::HeaderMap) -> bool {
    criteria.headers.iter().all(|m| {
        let result = match_header(m, headers);
        if m.invert_match { !result } else { result }
    })
}

fn match_header(hm: &HeaderMatcherConfig, headers: &http::HeaderMap) -> bool {
    let value = headers.get(&hm.name).and_then(|v| v.to_str().ok());

    match &hm.match_specifier {
        HeaderMatchSpecifierConfig::Present => value.is_some(),
        HeaderMatchSpecifierConfig::Exact(e) => value.is_some_and(|v| v == e),
        HeaderMatchSpecifierConfig::Prefix(p) => value.is_some_and(|v| v.starts_with(p.as_str())),
        HeaderMatchSpecifierConfig::Suffix(s) => value.is_some_and(|v| v.ends_with(s.as_str())),
        HeaderMatchSpecifierConfig::Contains(c) => value.is_some_and(|v| v.contains(c.as_str())),
        HeaderMatchSpecifierConfig::SafeRegex(re) => value.is_some_and(|v| re.is_match(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::route_config::{
        RouteConfig, RouteConfigAction, RouteConfigMatch, VirtualHostConfig,
    };

    fn simple_route(prefix: &str, cluster: &str) -> RouteConfig {
        RouteConfig {
            match_criteria: RouteConfigMatch {
                path_specifier: PathSpecifierConfig::Prefix(prefix.into()),
                headers: vec![],
                case_sensitive: true,
            },
            action: RouteConfigAction::Cluster(cluster.into()),
        }
    }

    fn simple_rc(virtual_hosts: Vec<VirtualHostConfig>) -> RouteConfigResource {
        RouteConfigResource {
            name: "test-rc".into(),
            virtual_hosts,
        }
    }

    #[test]
    fn domain_exact() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["foo.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        assert!(rc.route("foo.com", "/", &http::HeaderMap::new()).is_ok());
    }

    #[test]
    fn domain_case_insensitive() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["FOO.COM".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        assert!(rc.route("foo.com", "/", &http::HeaderMap::new()).is_ok());
    }

    #[test]
    fn domain_suffix_wildcard() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*.foo.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let h = http::HeaderMap::new();
        assert!(rc.route("bar.foo.com", "/", &h).is_ok());
        assert!(rc.route("foo.com", "/", &h).is_err());
    }

    #[test]
    fn domain_prefix_wildcard() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["foo.*".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let h = http::HeaderMap::new();
        assert!(rc.route("foo.bar", "/", &h).is_ok());
        assert!(rc.route("bar.foo", "/", &h).is_err());
    }

    #[test]
    fn domain_universal() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        assert!(
            rc.route("anything.com", "/", &http::HeaderMap::new())
                .is_ok()
        );
    }

    #[test]
    fn domain_exact_beats_suffix() {
        let rc = simple_rc(vec![
            VirtualHostConfig {
                name: "vh-suffix".into(),
                domains: vec!["*.foo.com".into()],
                routes: vec![simple_route("/", "cluster-suffix")],
            },
            VirtualHostConfig {
                name: "vh-exact".into(),
                domains: vec!["bar.foo.com".into()],
                routes: vec![simple_route("/", "cluster-exact")],
            },
        ]);
        let action = rc
            .route("bar.foo.com", "/", &http::HeaderMap::new())
            .unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-exact"));
    }

    #[test]
    fn domain_suffix_beats_universal() {
        let rc = simple_rc(vec![
            VirtualHostConfig {
                name: "vh-universal".into(),
                domains: vec!["*".into()],
                routes: vec![simple_route("/", "cluster-universal")],
            },
            VirtualHostConfig {
                name: "vh-suffix".into(),
                domains: vec!["*.foo.com".into()],
                routes: vec![simple_route("/", "cluster-suffix")],
            },
        ]);
        let action = rc
            .route("bar.foo.com", "/", &http::HeaderMap::new())
            .unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-suffix"));
    }

    #[test]
    fn domain_longer_suffix_wins() {
        let rc = simple_rc(vec![
            VirtualHostConfig {
                name: "vh-short".into(),
                domains: vec!["*.com".into()],
                routes: vec![simple_route("/", "cluster-short")],
            },
            VirtualHostConfig {
                name: "vh-long".into(),
                domains: vec!["*.foo.com".into()],
                routes: vec![simple_route("/", "cluster-long")],
            },
        ]);
        let action = rc
            .route("bar.foo.com", "/", &http::HeaderMap::new())
            .unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-long"));
    }

    #[test]
    fn domain_no_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["foo.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        assert!(rc.route("bar.com", "/", &http::HeaderMap::new()).is_err());
    }

    #[test]
    fn basic_routing() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/", "cluster-1")],
        }]);
        let headers = http::HeaderMap::new();

        let action = rc.route("any.host", "/foo", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-1"));
    }

    #[test]
    fn domain_selects_virtual_host() {
        let rc = simple_rc(vec![
            VirtualHostConfig {
                name: "vh-foo".into(),
                domains: vec!["foo.com".into()],
                routes: vec![simple_route("/", "cluster-foo")],
            },
            VirtualHostConfig {
                name: "vh-bar".into(),
                domains: vec!["bar.com".into()],
                routes: vec![simple_route("/", "cluster-bar")],
            },
        ]);
        let headers = http::HeaderMap::new();

        let action = rc.route("foo.com", "/x", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-foo"));

        let action = rc.route("bar.com", "/x", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-bar"));
    }

    #[test]
    fn no_matching_virtual_host() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["foo.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let headers = http::HeaderMap::new();

        let err = rc.route("unknown.com", "/", &headers).unwrap_err();
        assert!(matches!(err, RoutingError::NoMatchingVirtualHost(_)));
    }

    #[test]
    fn first_matching_route_wins() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![
                simple_route("/svc/", "cluster-svc"),
                simple_route("/", "cluster-default"),
            ],
        }]);
        let headers = http::HeaderMap::new();

        let action = rc.route("host", "/svc/Method", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-svc"));

        let action = rc.route("host", "/other", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-default"));
    }

    #[test]
    fn no_matching_route() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/svc/", "c1")],
        }]);
        let headers = http::HeaderMap::new();

        let err = rc.route("host", "/other", &headers).unwrap_err();
        assert!(matches!(err, RoutingError::NoMatchingRoute(_)));
    }

    #[test]
    fn exact_path_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::Path("/svc/Method".into()),
                    headers: vec![],
                    case_sensitive: true,
                },
                action: RouteConfigAction::Cluster("c1".into()),
            }],
        }]);
        let headers = http::HeaderMap::new();

        assert!(rc.route("host", "/svc/Method", &headers).is_ok());
        assert!(rc.route("host", "/svc/Other", &headers).is_err());
    }

    #[test]
    fn regex_path_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::SafeRegex(
                        regex::Regex::new("^/svc/.*").unwrap(),
                    ),
                    headers: vec![],
                    case_sensitive: true,
                },
                action: RouteConfigAction::Cluster("c1".into()),
            }],
        }]);
        let headers = http::HeaderMap::new();

        assert!(rc.route("host", "/svc/Anything", &headers).is_ok());
        assert!(rc.route("host", "/other", &headers).is_err());
    }

    #[test]
    fn header_matcher_filters_routes() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![HeaderMatcherConfig {
                            name: "x-env".into(),
                            match_specifier: HeaderMatchSpecifierConfig::Exact("prod".into()),
                            invert_match: false,
                        }],
                        case_sensitive: true,
                    },
                    action: RouteConfigAction::Cluster("cluster-prod".into()),
                },
                simple_route("/", "cluster-default"),
            ],
        }]);

        let mut prod_headers = http::HeaderMap::new();
        prod_headers.insert("x-env", "prod".parse().unwrap());
        let action = rc.route("host", "/", &prod_headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-prod"));

        let action = rc.route("host", "/", &http::HeaderMap::new()).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-default"));
    }

    #[test]
    fn weighted_clusters_passed_through() {
        use crate::xds::resource::route_config::WeightedCluster;
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::Prefix("/".into()),
                    headers: vec![],
                    case_sensitive: true,
                },
                action: RouteConfigAction::WeightedClusters(vec![
                    WeightedCluster {
                        name: "c1".into(),
                        weight: 70,
                    },
                    WeightedCluster {
                        name: "c2".into(),
                        weight: 30,
                    },
                ]),
            }],
        }]);
        let action = rc.route("host", "/", &http::HeaderMap::new()).unwrap();
        assert!(matches!(action, RouteConfigAction::WeightedClusters(wcs) if wcs.len() == 2));
    }
}
