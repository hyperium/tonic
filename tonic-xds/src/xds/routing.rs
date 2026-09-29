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

//! xDS routing: route matching logic and [`XdsRouter`] implementation.
//!
//! This module contains both the route matching logic (domain → path → headers)
//! and the stateful [`XdsRouter`] that subscribes to cache updates and serves
//! routing decisions.
//!
//! Domain matching follows gRFC A27 priority:
//! 1. Exact match
//! 2. Suffix wildcard (`*.foo.com`)
//! 3. Prefix wildcard (`foo.*`)
//! 4. Universal wildcard `*`
//!
//! Within each category, the most specific (longest non-wildcard part) wins.

use std::cmp::Reverse;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::sync::watch;

use crate::client::retry::{GrpcRetryClassifierFactory, RetryClassifierFactory};
use crate::client::route::{AcquiredConfig, RouteDecision, RouteInput, Router, RoutingSnapshot};
use crate::common::async_util::AbortOnDrop;
use crate::xds::cache::XdsCache;
use crate::xds::resource::hash_policy::HashPolicyConfig;
use crate::xds::resource::route_config::{
    HeaderMatchSpecifierConfig, HeaderMatcherConfig, PathSpecifierConfig, RouteConfig,
    RouteConfigAction, RouteConfigMatch, RouteConfigResource, VirtualHostConfig, WeightedCluster,
};
use crate::xds::resource::string_matcher::{
    ends_with_ignore_ascii_case, starts_with_ignore_ascii_case,
};

/// Default timeout for waiting for the initial route config (matches gRFC A57
/// resource initial timeout).
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// xDS-backed [`Router`] that resolves requests to cluster names.
///
/// Subscribes to route config changes from [`XdsCache`] via a background watch task
/// and maintains a shared [`ArcSwapOption`] for lock-free reads on the hot path.
/// The watch task is aborted when the router is dropped.
///
/// The first RPC blocks (up to [`DEFAULT_READY_TIMEOUT`]) until the initial route
/// config is available, matching standard gRPC behavior where RPCs wait for the
/// resolver's first update. Subsequent RPCs read the config lock-free.
pub(crate) struct XdsRouter {
    route_config: Arc<ArcSwapOption<RoutingSnapshot>>,
    ready_rx: watch::Receiver<bool>,
    _watch_task: AbortOnDrop,
}

impl XdsRouter {
    /// Creates a new `XdsRouter` that watches route config from the given cache,
    /// compiling per-route retry policies with the default
    /// [`GrpcRetryClassifierFactory`].
    ///
    /// Spawns a background task that updates the local route config whenever
    /// the cache publishes a new one. The task is aborted when this router
    /// is dropped.
    pub(crate) fn new(cache: &XdsCache) -> Self {
        Self::with_retry_factory(cache, Arc::new(GrpcRetryClassifierFactory))
    }

    /// Like [`new`](Self::new) but compiles per-route retry policies with a custom
    /// [`RetryClassifierFactory`], letting a non-gRPC transport interpret
    /// `retry_on` for that transport.
    pub(crate) fn with_retry_factory(
        cache: &XdsCache,
        retry_factory: Arc<dyn RetryClassifierFactory>,
    ) -> Self {
        let route_config = Arc::new(ArcSwapOption::empty());
        let (ready_tx, ready_rx) = watch::channel(false);
        let rc = route_config.clone();
        let mut watcher = cache.watch_route_config();
        let handle = tokio::spawn(async move {
            let mut ready_tx = Some(ready_tx);
            while let Some(config) = watcher.next().await {
                rc.store(Some(Arc::new(RoutingSnapshot::new(
                    config,
                    retry_factory.as_ref(),
                ))));
                // Signal readiness on the first config, then drop the sender.
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(true);
                }
            }
        });
        Self {
            route_config,
            ready_rx,
            _watch_task: AbortOnDrop(handle),
        }
    }

    /// The route config currently in effect, or `None` if none has arrived yet.
    pub(crate) fn snapshot(&self) -> Option<Arc<RoutingSnapshot>> {
        self.route_config.load_full()
    }
}

impl Router for XdsRouter {
    fn acquire(&self) -> AcquiredConfig {
        if let Some(config) = self.snapshot() {
            return AcquiredConfig::Ready(config);
        }
        // Wait for the initial route config, matching standard gRPC behavior
        // where RPCs block until the resolver provides the first update.
        let route_config_ref = self.route_config.clone();
        let mut ready_rx = self.ready_rx.clone();
        AcquiredConfig::Pending(Box::pin(async move {
            tokio::time::timeout(DEFAULT_READY_TIMEOUT, ready_rx.wait_for(|ready| *ready))
                .await
                .map_err(|_| RoutingError::NotReady)?
                .map_err(|_| RoutingError::NotReady)?;
            route_config_ref.load_full().ok_or(RoutingError::NotReady)
        }))
    }

    fn route(
        &self,
        input: &RouteInput<'_>,
        config: &RoutingSnapshot,
    ) -> Result<RouteDecision, RoutingError> {
        resolve_route(config, input.authority, input.path, input.headers)
    }
}

/// Resolve a route decision from the given config, authority, path, and headers.
fn resolve_route(
    rc: &RoutingSnapshot,
    authority: &str,
    path: &str,
    headers: &http::HeaderMap,
) -> Result<RouteDecision, RoutingError> {
    let route = rc.matched_route(authority, path, headers)?;
    let cluster = match &route.action {
        RouteConfigAction::Cluster(name) => name.clone(),
        RouteConfigAction::WeightedClusters(clusters) => select_weighted_cluster(clusters)
            .ok_or(RoutingError::EmptyWeightedClusters)?
            .to_string(),
    };

    // Retry config for the matched route, from the same snapshot we routed on so
    // retry and routing act on one RDS version.
    let retry_config = rc.retry_for(route.retry_config.as_ref());

    // gRFC A42 ring-hash request hash. The policy list is empty for now, so
    // `request_hash` resolves to `None` and the ring-hash picker falls back to a
    // random hash.
    // TODO(madhurishgupta): populate hash policies from the route's RDS
    // `RouteAction.hash_policy` (later PR).
    let policies: &[HashPolicyConfig] = &[];
    let request_hash = HashPolicyConfig::request_hash(headers, policies);

    Ok(RouteDecision {
        cluster,
        request_hash,
        retry_config,
    })
}

/// Error returned when routing fails.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum RoutingError {
    #[error("route config not yet available")]
    NotReady,
    #[error("no matching virtual host for authority '{0}'")]
    NoMatchingVirtualHost(String),
    #[error("no matching route in virtual host for path '{0}'")]
    NoMatchingRoute(String),
    #[error("weighted cluster selection failed (empty cluster list)")]
    EmptyWeightedClusters,
}

impl RouteConfigResource {
    /// Match a request and return the target cluster action.
    ///
    /// Performs domain matching on the authority, then walks routes in order
    /// to find the first match. Test-only convenience wrapper around
    /// [`matched_route`](Self::matched_route); production routing uses
    /// `matched_route` so it can also read the matched route's retry policy.
    #[cfg(test)]
    pub(crate) fn route(
        &self,
        authority: &str,
        path: &str,
        headers: &http::HeaderMap,
    ) -> Result<&RouteConfigAction, RoutingError> {
        self.matched_route(authority, path, headers)
            .map(|route| &route.action)
    }

    /// Match a request and return the full matched [`RouteConfig`].
    ///
    /// Performs domain matching on the authority, then walks routes in order to
    /// find the first match. Returns the whole matched route so callers can read
    /// per-route fields (e.g. the retry policy) alongside the action.
    pub(crate) fn matched_route(
        &self,
        authority: &str,
        path: &str,
        headers: &http::HeaderMap,
    ) -> Result<&RouteConfig, RoutingError> {
        let vh = find_best_matching_virtual_host(authority, &self.virtual_hosts)
            .ok_or_else(|| RoutingError::NoMatchingVirtualHost(authority.to_string()))?;

        for route in &vh.routes {
            if route_matches(route, path, headers) {
                return Ok(route);
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

    if authority.eq_ignore_ascii_case(pattern) {
        return Some(DomainMatchScore(
            DomainMatchType::Exact,
            Reverse(pattern.len()),
        ));
    }

    // The wildcard must absorb at least one authority byte, so the
    // authority has to be strictly longer than the pattern tail.
    if let Some(suffix) = pattern.strip_prefix(WILDCARD)
        && authority.len() > suffix.len()
        && ends_with_ignore_ascii_case(authority, suffix)
    {
        return Some(DomainMatchScore(
            DomainMatchType::Suffix,
            Reverse(suffix.len()),
        ));
    }

    if let Some(prefix) = pattern.strip_suffix(WILDCARD)
        && authority.len() > prefix.len()
        && starts_with_ignore_ascii_case(authority, prefix)
    {
        return Some(DomainMatchScore(
            DomainMatchType::Prefix,
            Reverse(prefix.len()),
        ));
    }

    None
}

fn route_matches(route: &RouteConfig, path: &str, headers: &http::HeaderMap) -> bool {
    match_path(&route.match_criteria, path)
        && match_headers(&route.match_criteria, headers)
        && match_fraction(route.match_criteria.match_fraction)
}

/// Probabilistic route gating per A28. Returns true if the route should be considered.
fn match_fraction(fraction: Option<u32>) -> bool {
    match fraction {
        None | Some(1_000_000..) => true,
        Some(0) => false,
        Some(n) => fastrand::u32(0..1_000_000) < n,
    }
}

/// Select a cluster from weighted clusters using accumulated weights and random selection.
///
/// Returns `None` if `clusters` is empty.
pub(crate) fn select_weighted_cluster(clusters: &[WeightedCluster]) -> Option<&str> {
    if clusters.is_empty() {
        return None;
    }

    let total: u64 = clusters.iter().map(|c| c.weight as u64).sum();
    if total == 0 {
        return Some(&clusters[fastrand::usize(0..clusters.len())].name);
    }

    let random = fastrand::u64(0..total);
    let mut acc = 0u64;
    for cluster in clusters {
        acc += cluster.weight as u64;
        if random < acc {
            return Some(&cluster.name);
        }
    }
    // random is in [0, total) and acc reaches total, so the loop always returns.
    unreachable!()
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

const DEFAULT_CONTENT_TYPE: &str = "application/grpc";

fn match_header(hm: &HeaderMatcherConfig, headers: &http::HeaderMap) -> bool {
    // Per A28: if content-type is not present, assume "application/grpc".
    let value = headers
        .get(&hm.name)
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            if hm.name.eq_ignore_ascii_case("content-type") {
                Some(DEFAULT_CONTENT_TYPE)
            } else {
                None
            }
        });

    match &hm.match_specifier {
        HeaderMatchSpecifierConfig::Present => value.is_some(),
        HeaderMatchSpecifierConfig::Absent => value.is_none(),
        HeaderMatchSpecifierConfig::String(sm) => value.is_some_and(|v| sm.is_match(v)),
        HeaderMatchSpecifierConfig::Range { start, end } => {
            value.is_some_and(|v| v.parse::<i64>().is_ok_and(|n| n >= *start && n < *end))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::cache::XdsCache;
    use crate::xds::resource::route_config::{
        RouteConfig, RouteConfigAction, RouteConfigMatch, VirtualHostConfig,
    };
    use crate::xds::resource::safe_regex::SafeRegex;
    use crate::xds::resource::string_matcher::StringMatcher;

    fn simple_route(prefix: &str, cluster: &str) -> RouteConfig {
        RouteConfig {
            match_criteria: RouteConfigMatch {
                path_specifier: PathSpecifierConfig::Prefix(prefix.into()),
                headers: vec![],
                case_sensitive: true,
                match_fraction: None,
            },
            action: RouteConfigAction::Cluster(cluster.into()),
            retry_config: None,
        }
    }

    fn simple_rc(virtual_hosts: Vec<VirtualHostConfig>) -> RouteConfigResource {
        RouteConfigResource {
            name: "test-rc".into(),
            virtual_hosts,
            metadata: Default::default(),
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
    fn domain_suffix_wildcard_case_insensitive() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*.FOO.com".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let h = http::HeaderMap::new();
        assert!(rc.route("bar.foo.com", "/", &h).is_ok());
        assert!(rc.route("BAR.foo.COM", "/", &h).is_ok());
        assert!(rc.route("foo.com", "/", &h).is_err());
    }

    #[test]
    fn domain_prefix_wildcard_case_insensitive() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["FOO.*".into()],
            routes: vec![simple_route("/", "c1")],
        }]);
        let h = http::HeaderMap::new();
        assert!(rc.route("foo.bar", "/", &h).is_ok());
        assert!(rc.route("FOO.bar", "/", &h).is_ok());
        assert!(rc.route("FOO.", "/", &h).is_err());
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
                    match_fraction: None,
                },
                action: RouteConfigAction::Cluster("c1".into()),
                retry_config: None,
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
                        SafeRegex::new("/svc/.*").unwrap(),
                    ),
                    headers: vec![],
                    case_sensitive: true,
                    match_fraction: None,
                },
                action: RouteConfigAction::Cluster("c1".into()),
                retry_config: None,
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
                            match_specifier: HeaderMatchSpecifierConfig::String(
                                StringMatcher::Exact {
                                    value: "prod".into(),
                                    ignore_case: false,
                                },
                            ),
                            invert_match: false,
                        }],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("cluster-prod".into()),
                    retry_config: None,
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
                    match_fraction: None,
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
                retry_config: None,
            }],
        }]);
        let action = rc.route("host", "/", &http::HeaderMap::new()).unwrap();
        assert!(matches!(action, RouteConfigAction::WeightedClusters(wcs) if wcs.len() == 2));
    }

    #[test]
    fn match_fraction_zero_never_matches() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![],
                        case_sensitive: true,
                        match_fraction: Some(0),
                    },
                    action: RouteConfigAction::Cluster("never".into()),
                    retry_config: None,
                },
                simple_route("/", "fallback"),
            ],
        }]);
        for _ in 0..100 {
            let action = rc.route("host", "/foo", &http::HeaderMap::new()).unwrap();
            assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "fallback"));
        }
    }

    #[test]
    fn match_fraction_million_always_matches() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::Prefix("/".into()),
                    headers: vec![],
                    case_sensitive: true,
                    match_fraction: Some(1_000_000),
                },
                action: RouteConfigAction::Cluster("always".into()),
                retry_config: None,
            }],
        }]);
        for _ in 0..100 {
            let action = rc.route("host", "/foo", &http::HeaderMap::new()).unwrap();
            assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "always"));
        }
    }

    #[test]
    fn range_match_header() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![HeaderMatcherConfig {
                            name: "x-version".into(),
                            match_specifier: HeaderMatchSpecifierConfig::Range {
                                start: 1,
                                end: 10,
                            },
                            invert_match: false,
                        }],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("versioned".into()),
                    retry_config: None,
                },
                simple_route("/", "default"),
            ],
        }]);

        let mut headers = http::HeaderMap::new();
        headers.insert("x-version", "5".parse().unwrap());
        let action = rc.route("host", "/", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "versioned"));

        // end is exclusive
        headers.insert("x-version", "10".parse().unwrap());
        let action = rc.route("host", "/", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "default"));

        // non-integer falls through
        headers.insert("x-version", "abc".parse().unwrap());
        let action = rc.route("host", "/", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "default"));

        // missing header falls through
        let action = rc.route("host", "/", &http::HeaderMap::new()).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "default"));
    }

    #[test]
    fn select_weighted_cluster_empty() {
        assert_eq!(select_weighted_cluster(&[]), None);
    }

    #[test]
    fn select_weighted_cluster_single() {
        let clusters = vec![WeightedCluster {
            name: "only".into(),
            weight: 100,
        }];
        assert_eq!(select_weighted_cluster(&clusters).unwrap(), "only");
    }

    #[test]
    fn select_weighted_cluster_zero_weights() {
        let clusters = vec![
            WeightedCluster {
                name: "a".into(),
                weight: 0,
            },
            WeightedCluster {
                name: "b".into(),
                weight: 0,
            },
        ];
        let name = select_weighted_cluster(&clusters).unwrap();
        assert!(name == "a" || name == "b");
    }

    #[test]
    fn select_weighted_cluster_weight_1_vs_0() {
        let clusters = vec![
            WeightedCluster {
                name: "winner".into(),
                weight: 1,
            },
            WeightedCluster {
                name: "loser".into(),
                weight: 0,
            },
        ];
        for _ in 0..1000 {
            assert_eq!(select_weighted_cluster(&clusters).unwrap(), "winner");
        }
    }

    #[test]
    fn content_type_defaults_to_grpc() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh1".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![HeaderMatcherConfig {
                            name: "content-type".into(),
                            match_specifier: HeaderMatchSpecifierConfig::String(
                                StringMatcher::Exact {
                                    value: "application/grpc".into(),
                                    ignore_case: false,
                                },
                            ),
                            invert_match: false,
                        }],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("grpc".into()),
                    retry_config: None,
                },
                simple_route("/", "fallback"),
            ],
        }]);
        // Per A28: content-type defaults to "application/grpc" when absent.
        let action = rc.route("host", "/", &http::HeaderMap::new()).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "grpc"));

        // Non-grpc content-type should not match the first route.
        let mut headers = http::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        let action = rc.route("host", "/", &headers).unwrap();
        assert!(matches!(action, RouteConfigAction::Cluster(c) if c == "fallback"));
    }

    #[test]
    fn ignore_case_exact_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh".into(),
            domains: vec!["*".into()],
            routes: vec![RouteConfig {
                match_criteria: RouteConfigMatch {
                    path_specifier: PathSpecifierConfig::Prefix("/".into()),
                    headers: vec![HeaderMatcherConfig {
                        name: "x-env".into(),
                        match_specifier: HeaderMatchSpecifierConfig::String(StringMatcher::Exact {
                            value: "Prod".into(),
                            ignore_case: true,
                        }),
                        invert_match: false,
                    }],
                    case_sensitive: true,
                    match_fraction: None,
                },
                action: RouteConfigAction::Cluster("matched".into()),
                retry_config: None,
            }],
        }]);

        let mut headers = http::HeaderMap::new();
        headers.insert("x-env", "prod".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        headers.insert("x-env", "PROD".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        headers.insert("x-env", "staging".parse().unwrap());
        assert!(rc.route("host", "/", &headers).is_err());
    }

    #[test]
    fn ignore_case_prefix_suffix_contains() {
        let make_route = |specifier: HeaderMatchSpecifierConfig| -> RouteConfigResource {
            simple_rc(vec![VirtualHostConfig {
                name: "vh".into(),
                domains: vec!["*".into()],
                routes: vec![
                    RouteConfig {
                        match_criteria: RouteConfigMatch {
                            path_specifier: PathSpecifierConfig::Prefix("/".into()),
                            headers: vec![HeaderMatcherConfig {
                                name: "x-tag".into(),
                                match_specifier: specifier,
                                invert_match: false,
                            }],
                            case_sensitive: true,
                            match_fraction: None,
                        },
                        action: RouteConfigAction::Cluster("matched".into()),
                        retry_config: None,
                    },
                    simple_route("/", "fallback"),
                ],
            }])
        };

        let mut headers = http::HeaderMap::new();

        let rc = make_route(HeaderMatchSpecifierConfig::String(StringMatcher::Prefix {
            value: "App".into(),
            ignore_case: true,
        }));
        headers.insert("x-tag", "APPLICATION/JSON".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        let rc = make_route(HeaderMatchSpecifierConfig::String(StringMatcher::Suffix {
            value: "JSON".into(),
            ignore_case: true,
        }));
        headers.insert("x-tag", "application/json".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        let rc = make_route(HeaderMatchSpecifierConfig::String(
            StringMatcher::Contains {
                value: "Grpc".into(),
                ignore_case: true,
            },
        ));
        headers.insert("x-tag", "APPLICATION/GRPC+PROTO".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );
    }

    #[test]
    fn safe_regex_header_match() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![HeaderMatcherConfig {
                            name: "x-tag".into(),
                            match_specifier: HeaderMatchSpecifierConfig::String(
                                StringMatcher::SafeRegex(SafeRegex::new("v[0-9]+").unwrap()),
                            ),
                            invert_match: false,
                        }],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("matched".into()),
                    retry_config: None,
                },
                simple_route("/", "fallback"),
            ],
        }]);

        let mut headers = http::HeaderMap::new();

        headers.insert("x-tag", "v123".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        headers.insert("x-tag", "latest".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "fallback")
        );
    }

    #[test]
    fn ignore_case_false_is_case_sensitive() {
        let rc = simple_rc(vec![VirtualHostConfig {
            name: "vh".into(),
            domains: vec!["*".into()],
            routes: vec![
                RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".into()),
                        headers: vec![HeaderMatcherConfig {
                            name: "x-env".into(),
                            match_specifier: HeaderMatchSpecifierConfig::String(
                                StringMatcher::Exact {
                                    value: "Prod".into(),
                                    ignore_case: false,
                                },
                            ),
                            invert_match: false,
                        }],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("matched".into()),
                    retry_config: None,
                },
                simple_route("/", "fallback"),
            ],
        }]);

        let mut headers = http::HeaderMap::new();
        headers.insert("x-env", "Prod".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "matched")
        );

        headers.insert("x-env", "prod".parse().unwrap());
        assert!(
            matches!(rc.route("host", "/", &headers).unwrap(), RouteConfigAction::Cluster(c) if c == "fallback")
        );
    }

    fn make_route_config(cluster: &str) -> Arc<RouteConfigResource> {
        Arc::new(simple_rc(vec![VirtualHostConfig {
            name: "vh".into(),
            domains: vec!["*".into()],
            routes: vec![simple_route("/", cluster)],
        }]))
    }

    #[tokio::test]
    async fn xds_router_routes_to_correct_cluster() {
        let cache = XdsCache::new();
        cache.update_route_config(make_route_config("my-cluster"));

        let router = XdsRouter::new(&cache);
        tokio::task::yield_now().await; // let watch task propagate

        let headers = http::HeaderMap::new();
        let input = RouteInput {
            authority: "my-service",
            path: "/",
            headers: &headers,
        };
        let config = router.snapshot().expect("config");
        let decision = router.route(&input, &config).unwrap();
        assert_eq!(decision.cluster, "my-cluster");
    }

    #[tokio::test]
    async fn xds_router_updates_on_config_change() {
        let cache = XdsCache::new();
        cache.update_route_config(make_route_config("cluster-a"));

        let router = XdsRouter::new(&cache);
        tokio::task::yield_now().await;

        let headers = http::HeaderMap::new();
        let input = RouteInput {
            authority: "svc",
            path: "/",
            headers: &headers,
        };

        let config = router.snapshot().expect("config");
        let decision = router.route(&input, &config).unwrap();
        assert_eq!(decision.cluster, "cluster-a");

        cache.update_route_config(make_route_config("cluster-b"));
        tokio::task::yield_now().await;

        let config = router.snapshot().expect("config");
        let decision = router.route(&input, &config).unwrap();
        assert_eq!(decision.cluster, "cluster-b");
    }

    #[tokio::test(start_paused = true)]
    async fn xds_router_returns_not_ready_without_config() {
        let cache = XdsCache::new();
        let router = XdsRouter::new(&cache);

        // With no config yet, `acquire` yields the waiting variant, which
        // blocks and then reports NotReady.
        assert!(router.snapshot().is_none());
        assert!(matches!(router.acquire(), AcquiredConfig::Pending(_)));

        let start = tokio::time::Instant::now();
        let result = router.acquire().get().await;

        assert!(
            matches!(result, Err(RoutingError::NotReady)),
            "expected NotReady, got {result:?}",
        );
        assert!(
            start.elapsed() >= DEFAULT_READY_TIMEOUT,
            "expected the wait to span the full ready timeout, took {:?}",
            start.elapsed(),
        );
    }
}
