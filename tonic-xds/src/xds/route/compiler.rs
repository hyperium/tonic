//! Compiles a [`RouteConfigResource`] into an efficient per-request matching structure.
//!
//! Compilation happens once at RDS update time — regex patterns are pre-compiled
//! and the result is a [`CompiledRouteConfig`] that supports fast,
//! allocation-free matching on the request hot path.

use crate::xds::resource::route_config::{
    HeaderMatchSpecifierConfig, HeaderMatcherConfig, PathSpecifierConfig, RouteConfigAction,
    RouteConfigResource, VirtualHostConfig,
};
use crate::xds::route::matcher;

/// A compiled route configuration ready for per-request matching.
///
/// Built from a [`RouteConfigResource`] at RDS update time.
/// Regex patterns are pre-compiled so the per-request path
/// does no allocation or compilation.
#[derive(Debug, Clone)]
pub(crate) struct CompiledRouteConfig {
    virtual_hosts: Vec<CompiledVirtualHost>,
}

#[derive(Debug, Clone)]
struct CompiledVirtualHost {
    domains: Vec<String>,
    routes: Vec<CompiledRoute>,
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    path_matcher: CompiledPathMatcher,
    header_matchers: Vec<CompiledHeaderMatcher>,
    case_sensitive: bool,
    action: RouteConfigAction,
}

#[derive(Debug, Clone)]
enum CompiledPathMatcher {
    Prefix(String),
    Path(String),
    SafeRegex(regex::Regex),
}

#[derive(Debug, Clone)]
struct CompiledHeaderMatcher {
    name: String,
    specifier: CompiledHeaderSpecifier,
    invert_match: bool,
}

#[derive(Debug, Clone)]
enum CompiledHeaderSpecifier {
    Exact(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
    SafeRegex(regex::Regex),
    Present,
}

/// Error returned when route matching fails.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum RoutingError {
    #[error("no matching virtual host for authority '{0}'")]
    NoMatchingVirtualHost(String),
    #[error("no matching route in virtual host for path '{0}'")]
    NoMatchingRoute(String),
}

impl CompiledRouteConfig {
    /// Compile a [`RouteConfigResource`] into a [`CompiledRouteConfig`].
    ///
    /// Returns an error if any regex pattern is invalid.
    pub(crate) fn compile(resource: &RouteConfigResource) -> Result<Self, regex::Error> {
        let virtual_hosts = resource
            .virtual_hosts
            .iter()
            .map(compile_virtual_host)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { virtual_hosts })
    }

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
        let vh = matcher::find_best_matching_virtual_host(authority, &self.virtual_hosts, |vh| {
            &vh.domains
        })
        .ok_or_else(|| RoutingError::NoMatchingVirtualHost(authority.to_string()))?;

        for route in &vh.routes {
            if route.matches(path, headers) {
                return Ok(&route.action);
            }
        }

        Err(RoutingError::NoMatchingRoute(path.to_string()))
    }
}

impl CompiledRoute {
    fn matches(&self, path: &str, headers: &http::HeaderMap) -> bool {
        self.match_path(path) && self.match_headers(headers)
    }

    fn match_path(&self, path: &str) -> bool {
        match &self.path_matcher {
            CompiledPathMatcher::Prefix(prefix) => {
                if prefix.is_empty() {
                    return true;
                }
                if self.case_sensitive {
                    path.starts_with(prefix.as_str())
                } else {
                    path.to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase())
                }
            }
            CompiledPathMatcher::Path(exact) => {
                if self.case_sensitive {
                    path == exact
                } else {
                    path.eq_ignore_ascii_case(exact)
                }
            }
            CompiledPathMatcher::SafeRegex(re) => re.is_match(path),
        }
    }

    fn match_headers(&self, headers: &http::HeaderMap) -> bool {
        self.header_matchers.iter().all(|m| {
            let result = m.matches(headers);
            if m.invert_match {
                !result
            } else {
                result
            }
        })
    }
}

impl CompiledHeaderMatcher {
    fn matches(&self, headers: &http::HeaderMap) -> bool {
        let value = headers.get(&self.name).and_then(|v| v.to_str().ok());

        match &self.specifier {
            CompiledHeaderSpecifier::Present => value.is_some(),
            CompiledHeaderSpecifier::Exact(e) => value.is_some_and(|v| v == e),
            CompiledHeaderSpecifier::Prefix(p) => value.is_some_and(|v| v.starts_with(p.as_str())),
            CompiledHeaderSpecifier::Suffix(s) => value.is_some_and(|v| v.ends_with(s.as_str())),
            CompiledHeaderSpecifier::Contains(c) => value.is_some_and(|v| v.contains(c.as_str())),
            CompiledHeaderSpecifier::SafeRegex(re) => value.is_some_and(|v| re.is_match(v)),
        }
    }
}

fn compile_virtual_host(vh: &VirtualHostConfig) -> Result<CompiledVirtualHost, regex::Error> {
    let routes = vh
        .routes
        .iter()
        .map(compile_route)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CompiledVirtualHost {
        domains: vh.domains.clone(),
        routes,
    })
}

fn compile_route(
    route: &crate::xds::resource::route_config::RouteConfig,
) -> Result<CompiledRoute, regex::Error> {
    let path_matcher = compile_path_matcher(&route.match_criteria.path_specifier)?;
    let header_matchers = route
        .match_criteria
        .headers
        .iter()
        .map(compile_header_matcher)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CompiledRoute {
        path_matcher,
        header_matchers,
        case_sensitive: route.match_criteria.case_sensitive,
        action: route.action.clone(),
    })
}

fn compile_path_matcher(spec: &PathSpecifierConfig) -> Result<CompiledPathMatcher, regex::Error> {
    match spec {
        PathSpecifierConfig::Prefix(p) => Ok(CompiledPathMatcher::Prefix(p.clone())),
        PathSpecifierConfig::Path(p) => Ok(CompiledPathMatcher::Path(p.clone())),
        PathSpecifierConfig::SafeRegex(pattern) => {
            let re = regex::Regex::new(pattern)?;
            Ok(CompiledPathMatcher::SafeRegex(re))
        }
    }
}

fn compile_header_matcher(hm: &HeaderMatcherConfig) -> Result<CompiledHeaderMatcher, regex::Error> {
    let specifier = compile_header_specifier(&hm.match_specifier)?;
    Ok(CompiledHeaderMatcher {
        name: hm.name.clone(),
        specifier,
        invert_match: hm.invert_match,
    })
}

fn compile_header_specifier(
    spec: &HeaderMatchSpecifierConfig,
) -> Result<CompiledHeaderSpecifier, regex::Error> {
    match spec {
        HeaderMatchSpecifierConfig::Exact(v) => Ok(CompiledHeaderSpecifier::Exact(v.clone())),
        HeaderMatchSpecifierConfig::Prefix(v) => Ok(CompiledHeaderSpecifier::Prefix(v.clone())),
        HeaderMatchSpecifierConfig::Suffix(v) => Ok(CompiledHeaderSpecifier::Suffix(v.clone())),
        HeaderMatchSpecifierConfig::Contains(v) => Ok(CompiledHeaderSpecifier::Contains(v.clone())),
        HeaderMatchSpecifierConfig::SafeRegex(pattern) => {
            let re = regex::Regex::new(pattern)?;
            Ok(CompiledHeaderSpecifier::SafeRegex(re))
        }
        HeaderMatchSpecifierConfig::Present => Ok(CompiledHeaderSpecifier::Present),
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
    fn basic_routing() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/", "cluster-1")],
        }]);
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        let action = compiled.route("any.host", "/foo", &headers).unwrap();
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
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        let action = compiled.route("foo.com", "/x", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-foo"));

        let action = compiled.route("bar.com", "/x", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-bar"));
    }

    #[test]
    fn no_matching_virtual_host() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["foo.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        let err = compiled.route("unknown.com", "/", &headers).unwrap_err();
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
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        let action = compiled.route("host", "/svc/Method", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-svc"));

        let action = compiled.route("host", "/other", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-default"));
    }

    #[test]
    fn no_matching_route() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/svc/", "c1")],
        }]);
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        let err = compiled.route("host", "/other", &headers).unwrap_err();
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
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        assert!(compiled.route("host", "/svc/Method", &headers).is_ok());
        assert!(compiled.route("host", "/svc/Other", &headers).is_err());
    }

    #[test]
    fn regex_path_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::SafeRegex("^/svc/.*".into()),
                    headers: vec![],
                    case_sensitive: true,
                },
                action: RouteConfigAction::Cluster("c1".into()),
            }],
        }]);
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let headers = http::HeaderMap::new();

        assert!(compiled.route("host", "/svc/Anything", &headers).is_ok());
        assert!(compiled.route("host", "/other", &headers).is_err());
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
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();

        let mut prod_headers = http::HeaderMap::new();
        prod_headers.insert("x-env", "prod".parse().unwrap());
        let action = compiled.route("host", "/", &prod_headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-prod"));

        let action = compiled
            .route("host", "/", &http::HeaderMap::new())
            .unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "cluster-default"));
    }

    #[test]
    fn invalid_regex_fails_compilation() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::SafeRegex("[invalid".into()),
                    headers: vec![],
                    case_sensitive: true,
                },
                action: RouteConfigAction::Cluster("c1".into()),
            }],
        }]);
        assert!(CompiledRouteConfig::compile(&rc).is_err());
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
        let compiled = CompiledRouteConfig::compile(&rc).unwrap();
        let action = compiled
            .route("host", "/", &http::HeaderMap::new())
            .unwrap();
        assert!(matches!(action, RouteConfigAction::WeightedClusters(wcs) if wcs.len() == 2));
    }
}
