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

//! Validated RouteConfiguration resource (RDS).
//!
//! Models the validated data shapes only (gRFC A28).

// Matching requests against these routes (path, header and fraction matching,
// with `-bin` headers treated as absent) is implemented in `routing/route.rs`.

use std::collections::HashSet;

use protobuf::Parse;
use xds_client::Error;
use xds_client::Resource;
use xds_client::resource::TypeUrl;

use super::safe_regex::SafeRegex;
use super::string_matcher::StringMatcher;
use crate::generated::envoy::config::route::v3::HeaderMatcherView;
use crate::generated::envoy::config::route::v3::RouteActionView;
use crate::generated::envoy::config::route::v3::RouteConfiguration;
use crate::generated::envoy::config::route::v3::RouteMatchView;
use crate::generated::envoy::config::route::v3::RouteView;
use crate::generated::envoy::config::route::v3::VirtualHostView;
use crate::generated::envoy::config::route::v3::header_matcher::HeaderMatchSpecifierOneof;
use crate::generated::envoy::config::route::v3::route::ActionOneof;
use crate::generated::envoy::config::route::v3::route_action::ClusterSpecifierOneof;
use crate::generated::envoy::config::route::v3::route_match::PathSpecifierOneof;
use crate::generated::envoy::r#type::v3::fractional_percent::DenominatorType;

/// Validated RouteConfiguration.
#[derive(Debug, Clone)]
pub(crate) struct RouteConfigResource {
    pub(crate) name: String,
    pub(crate) virtual_hosts: Vec<VirtualHost>,
}

/// Validated virtual host with domain matching and routes.
#[derive(Debug, Clone)]
pub(crate) struct VirtualHost {
    pub(crate) name: String,
    pub(crate) domains: Vec<DomainMatcher>,
    pub(crate) routes: Vec<Route>,
}

/// A validated virtual-host domain pattern (gRFC A27).
///
/// Built with [`DomainMatcher::try_from`], which lowercases the domain and
/// removes the `*`, so the stored literal is ASCII lowercase and, for
/// wildcard variants, non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DomainMatcher {
    /// `example.com`
    Exact(String),
    /// `*.example.com`, stored as `.example.com`.
    Suffix(String),
    /// `example.*`, stored as `example.`.
    Prefix(String),
    /// `*`
    Universal,
}

/// An empty domain pattern or one with unsupported wildcard placement.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidDomainPattern;

impl TryFrom<&str> for DomainMatcher {
    type Error = InvalidDomainPattern;

    fn try_from(domain: &str) -> Result<Self, Self::Error> {
        if domain.is_empty() {
            return Err(InvalidDomainPattern);
        }
        let domain = domain.to_ascii_lowercase();
        match domain.find('*') {
            None => Ok(Self::Exact(domain)),
            Some(0) if domain.len() == 1 => Ok(Self::Universal),
            Some(0) if !domain[1..].contains('*') => Ok(Self::Suffix(domain[1..].to_string())),
            Some(index) if index == domain.len() - 1 => {
                Ok(Self::Prefix(domain[..index].to_string()))
            }
            _ => Err(InvalidDomainPattern),
        }
    }
}

/// A validated route with match criteria and action.
#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub(crate) route_match: RouteMatch,
    pub(crate) action: RouteAction,
}

/// Validated route match criteria.
#[derive(Debug, Clone)]
pub(crate) struct RouteMatch {
    pub(crate) path_specifier: PathSpecifier,
    pub(crate) headers: Vec<HeaderMatcher>,
    pub(crate) case_sensitive: bool,
    /// Fraction of requests this route should match, as numerator out of
    /// 1,000,000. `None` means always match (100%).
    pub(crate) match_fraction: Option<u32>,
}

/// Path matching specifier.
#[derive(Debug, Clone)]
pub(crate) enum PathSpecifier {
    Prefix(String),
    Path(String),
    SafeRegex(SafeRegex),
}

/// Header matching criteria.
#[derive(Debug, Clone)]
pub(crate) struct HeaderMatcher {
    /// The header name, lowercased. Metadata keys are always lowercase, so
    /// lowercasing once here lets header names match case-insensitively
    /// without lowercasing on every RPC.
    pub(crate) name: String,
    pub(crate) match_specifier: HeaderMatchSpecifier,
    pub(crate) invert_match: bool,
}

/// Header match specifier variants.
///
/// The `String` variant carries a generic [`StringMatcher`] (exact / prefix /
/// suffix / contains / safe_regex, with optional ASCII case-insensitive
/// matching per gRFC A63). `Present`, `Absent`, and `Range` are header-specific
/// extensions beyond the generic StringMatcher.
#[derive(Debug, Clone)]
pub(crate) enum HeaderMatchSpecifier {
    String(StringMatcher),
    /// Match if header is present (any value).
    Present,
    /// Match if header is absent.
    Absent,
    /// Match if the header value, parsed as an integer, falls within `[start, end)`.
    Range {
        start: i64,
        end: i64,
    },
}

/// Route action deciding where to send traffic.
#[derive(Debug, Clone)]
pub(crate) enum RouteAction {
    Cluster(String),
    WeightedClusters(Vec<WeightedCluster>),
}

/// A cluster with an associated weight for traffic splitting.
#[derive(Debug, Clone)]
pub(crate) struct WeightedCluster {
    pub(crate) name: String,
    pub(crate) weight: u32,
}

impl Resource for RouteConfigResource {
    type Message = RouteConfiguration;

    const TYPE_URL: TypeUrl =
        TypeUrl::new("type.googleapis.com/envoy.config.route.v3.RouteConfiguration");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

    fn deserialize(bytes: bytes::Bytes) -> xds_client::Result<Self::Message> {
        RouteConfiguration::parse(&bytes)
            .map_err(|e| Error::Validation(format!("failed to decode RouteConfiguration: {e}")))
    }

    fn name(message: &Self::Message) -> &str {
        message.name().to_str().unwrap_or_default()
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name().to_str().unwrap_or_default().to_string();

        let virtual_hosts_view = message.virtual_hosts();
        let mut virtual_hosts = Vec::new();
        for vh in virtual_hosts_view.iter() {
            virtual_hosts.push(validate_virtual_host(vh)?);
        }

        Ok(RouteConfigResource {
            name,
            virtual_hosts,
        })
    }
}

fn validate_virtual_host(vh: VirtualHostView<'_>) -> xds_client::Result<VirtualHost> {
    let name = vh.name().to_str().unwrap_or_default().to_string();

    let domains_view = vh.domains();
    if domains_view.is_empty() {
        return Err(Error::Validation(format!(
            "virtual host '{name}' has no domains"
        )));
    }
    let domains = domains_view
        .iter()
        .map(|d| {
            // Invalid UTF-8 is not expected in practice; xDS resource parsing
            // will later convert panics like this into validation errors.
            let domain = d
                .to_str()
                .expect("virtual host domain should be valid UTF-8");
            DomainMatcher::try_from(domain).map_err(|_| {
                Error::Validation(format!(
                    "invalid domain pattern '{domain}' in virtual host '{name}'"
                ))
            })
        })
        .collect::<xds_client::Result<Vec<_>>>()?;

    let mut routes = Vec::new();
    for route in vh.routes().iter() {
        if let Some(validated) = validate_route(route)? {
            routes.push(validated);
        }
    }

    Ok(VirtualHost {
        name,
        domains,
        routes,
    })
}

/// Returns `Ok(None)` for routes that should be silently skipped (query
/// param matchers, unsupported cluster specifiers like `cluster_header`),
/// per gRFC A28.
fn validate_route(route: RouteView<'_>) -> xds_client::Result<Option<Route>> {
    if !route.has_match() {
        return Err(Error::Validation("route missing match field".into()));
    }
    let route_match = route.r#match();

    // Per A28: ignore routes with query parameter matchers.
    if !route_match.query_parameters().is_empty() {
        return Ok(None);
    }

    let match_criteria = validate_route_match(route_match)?;

    let action = match route.action() {
        ActionOneof::Route(route_action) => match validate_route_action(route_action)? {
            Some(action) => action,
            None => return Ok(None),
        },
        // Per A28: action field must be "route", otherwise NACK.
        _ => {
            return Err(Error::Validation(
                "only route action is supported for client routing".into(),
            ));
        }
    };

    Ok(Some(Route {
        route_match: match_criteria,
        action,
    }))
}

fn validate_route_match(rm: RouteMatchView<'_>) -> xds_client::Result<RouteMatch> {
    let path_specifier = match rm.path_specifier() {
        PathSpecifierOneof::Prefix(p) => {
            PathSpecifier::Prefix(p.to_str().unwrap_or_default().to_string())
        }
        PathSpecifierOneof::Path(p) => {
            PathSpecifier::Path(p.to_str().unwrap_or_default().to_string())
        }
        PathSpecifierOneof::SafeRegex(r) => {
            let pattern = r.regex();
            let pattern = pattern.to_str().unwrap_or_default();
            let regex = pattern
                .parse()
                .map_err(|e| Error::Validation(format!("invalid path regex '{pattern}': {e}")))?;
            PathSpecifier::SafeRegex(regex)
        }
        // Per A28: not having path_specifier will cause a NACK.
        PathSpecifierOneof::not_set(_) => {
            return Err(Error::Validation(
                "route match missing path_specifier".into(),
            ));
        }
        _ => {
            return Err(Error::Validation(
                "unsupported path specifier variant".into(),
            ));
        }
    };

    let case_sensitive = if rm.has_case_sensitive() {
        rm.case_sensitive().value()
    } else {
        true
    };

    // Per A28, a matcher naming a `-bin` header must behave as if that header
    // were absent, so the matcher is kept here and neutralized at match time.
    // Dropping it instead would widen the route to traffic it must not match.
    let headers = rm
        .headers()
        .iter()
        .map(validate_header_matcher)
        .collect::<xds_client::Result<Vec<_>>>()?;

    // Per A28: use runtime_fraction.default_value, normalize to numerator out
    // of 1,000,000. runtime_key is ignored (gRPC has no runtime config).
    let match_fraction = match rm.runtime_fraction_opt() {
        None => None,
        Some(rf) => {
            if !rf.has_default_value() {
                return Err(Error::Validation(
                    "runtime_fraction is missing its required default_value".into(),
                ));
            }
            let frac = rf.default_value();
            let scale = match frac.denominator() {
                DenominatorType::Hundred => 10_000,
                DenominatorType::TenThousand => 100,
                DenominatorType::Million => 1,
                _ => 1,
            };
            Some(frac.numerator().saturating_mul(scale).min(1_000_000))
        }
    };

    Ok(RouteMatch {
        path_specifier,
        headers,
        case_sensitive,
        match_fraction,
    })
}

fn validate_header_matcher(hm: HeaderMatcherView<'_>) -> xds_client::Result<HeaderMatcher> {
    let name = hm.name().to_str().unwrap_or_default().to_ascii_lowercase();
    if name.is_empty() {
        return Err(Error::Validation("header matcher name is empty".into()));
    }

    // Legacy matchers are deprecated in favor of StringMatch but remain
    // widely used and are still required by gRFC A63.
    // Invalid UTF-8 is not expected in practice; xDS resource parsing will
    // later convert the `expect` panics below into validation errors.
    #[allow(deprecated, unreachable_patterns)]
    let match_specifier = match hm.header_match_specifier() {
        HeaderMatchSpecifierOneof::ExactMatch(v) => {
            let value = v
                .to_str()
                .expect("exact header matcher should be valid UTF-8");
            HeaderMatchSpecifier::String(StringMatcher::exact(value, false))
        }
        HeaderMatchSpecifierOneof::SafeRegexMatch(r) => {
            let pattern = r.regex();
            let pattern = pattern
                .to_str()
                .expect("header regex should be valid UTF-8");
            HeaderMatchSpecifier::String(StringMatcher::safe_regex(pattern.parse().map_err(
                |e| Error::Validation(format!("invalid header regex '{pattern}': {e}")),
            )?))
        }
        HeaderMatchSpecifierOneof::RangeMatch(r) => HeaderMatchSpecifier::Range {
            start: r.start(),
            end: r.end(),
        },
        HeaderMatchSpecifierOneof::PresentMatch(present) => {
            if present {
                HeaderMatchSpecifier::Present
            } else {
                HeaderMatchSpecifier::Absent
            }
        }
        HeaderMatchSpecifierOneof::PrefixMatch(v) => {
            let value = v
                .to_str()
                .expect("prefix header matcher should be valid UTF-8");
            HeaderMatchSpecifier::String(StringMatcher::prefix(value, false)?)
        }
        HeaderMatchSpecifierOneof::SuffixMatch(v) => {
            let value = v
                .to_str()
                .expect("suffix header matcher should be valid UTF-8");
            HeaderMatchSpecifier::String(StringMatcher::suffix(value, false)?)
        }
        HeaderMatchSpecifierOneof::ContainsMatch(v) => {
            let value = v
                .to_str()
                .expect("contains header matcher should be valid UTF-8");
            HeaderMatchSpecifier::String(StringMatcher::contains(value, false)?)
        }
        HeaderMatchSpecifierOneof::StringMatch(sm) => {
            HeaderMatchSpecifier::String(StringMatcher::from_proto(sm)?)
        }
        HeaderMatchSpecifierOneof::not_set(_) => HeaderMatchSpecifier::Present,
        _ => {
            return Err(Error::Validation(
                "unsupported header match specifier".into(),
            ));
        }
    };

    Ok(HeaderMatcher {
        name,
        match_specifier,
        invert_match: hm.invert_match(),
    })
}

/// Returns `Ok(None)` for routes whose cluster specifier is unsupported
/// (e.g. `cluster_header`) or unset, both of which A28 requires be skipped.
fn validate_route_action(ra: RouteActionView<'_>) -> xds_client::Result<Option<RouteAction>> {
    match ra.cluster_specifier() {
        ClusterSpecifierOneof::Cluster(name) => {
            let name = name.to_str().unwrap_or_default();
            if name.is_empty() {
                return Err(Error::Validation("cluster name is empty".into()));
            }
            Ok(Some(RouteAction::Cluster(name.to_string())))
        }
        ClusterSpecifierOneof::WeightedClusters(wc) => {
            let clusters_view = wc.clusters();
            if clusters_view.is_empty() {
                return Err(Error::Validation("weighted_clusters is empty".into()));
            }
            let mut clusters = Vec::new();
            let mut total_weight: u64 = 0;
            for c in clusters_view.iter() {
                // Per A28: zero-weight entries never receive traffic, so drop them.
                let weight = c.weight_opt().map(|w| w.value()).unwrap_or(0);
                if weight == 0 {
                    continue;
                }
                let name = c.name().to_str().unwrap_or_default();
                if name.is_empty() {
                    return Err(Error::Validation("weighted cluster name is empty".into()));
                }
                total_weight += u64::from(weight);
                if total_weight > u64::from(u32::MAX) {
                    return Err(Error::Validation(format!(
                        "sum of weighted cluster weights exceeds {}",
                        u32::MAX
                    )));
                }
                clusters.push(WeightedCluster {
                    name: name.to_string(),
                    weight,
                });
            }
            // Per A28: the weights must add up to a non-zero total, otherwise
            // there is nothing for a picker to distribute traffic across.
            if clusters.is_empty() {
                return Err(Error::Validation(
                    "weighted_clusters has no cluster with a non-zero weight".into(),
                ));
            }
            Ok(Some(RouteAction::WeightedClusters(clusters)))
        }
        // Per A28: ignore the route when the cluster specifier is unsupported
        // (e.g. `cluster_header`) or unset. A specifier field added to the oneof
        // by a newer control plane decodes as an unknown field, which protobuf
        // reports as `not_set`, so that case must be ignored rather than NACKed.
        _ => Ok(None),
    }
}

impl VirtualHost {
    /// Returns cluster names referenced by this virtual host, for cascading
    /// CDS subscriptions after the dependency manager selects the host that
    /// matches the channel authority.
    #[allow(dead_code)] // TODO: remove once dependency manager calls this.
    pub(crate) fn cluster_names(&self) -> HashSet<String> {
        let mut clusters = HashSet::new();
        for route in &self.routes {
            match &route.action {
                RouteAction::Cluster(name) => {
                    clusters.insert(name.clone());
                }
                RouteAction::WeightedClusters(wcs) => {
                    for wc in wcs {
                        clusters.insert(wc.name.clone());
                    }
                }
            }
        }
        clusters
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;

    use super::*;
    use crate::generated::envoy::config::core::v3::RuntimeFractionalPercent;
    use crate::generated::envoy::config::route::v3::HeaderMatcher as EnvoyHeaderMatcher;
    use crate::generated::envoy::config::route::v3::QueryParameterMatcher;
    use crate::generated::envoy::config::route::v3::RedirectAction;
    use crate::generated::envoy::config::route::v3::Route as EnvoyRoute;
    use crate::generated::envoy::config::route::v3::RouteAction as EnvoyRouteAction;
    use crate::generated::envoy::config::route::v3::RouteMatch as EnvoyRouteMatch;
    use crate::generated::envoy::config::route::v3::VirtualHost as EnvoyVirtualHost;
    use crate::generated::envoy::config::route::v3::WeightedCluster as EnvoyWeightedCluster;
    use crate::generated::envoy::config::route::v3::weighted_cluster::ClusterWeight;
    use crate::generated::envoy::r#type::matcher::v3::RegexMatcher;
    use crate::generated::envoy::r#type::matcher::v3::StringMatcher as EnvoyStringMatcher;
    use crate::generated::envoy::r#type::v3::FractionalPercent;
    use protobuf::Serialize;
    use protobuf_well_known_types::UInt32Value;

    fn make_route(prefix: &str, cluster: &str) -> EnvoyRoute {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix(prefix);

        let mut route_action = EnvoyRouteAction::new();
        route_action.set_cluster(cluster);

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);
        route
    }

    fn make_route_config(name: &str) -> RouteConfiguration {
        wrap_route(name, make_route("/", "cluster-1"))
    }

    fn wrap_route(name: &str, route: EnvoyRoute) -> RouteConfiguration {
        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut rc = RouteConfiguration::new();
        rc.set_name(name);
        rc.virtual_hosts_mut().push(vh);
        rc
    }

    fn route_config_with_header(header: EnvoyHeaderMatcher) -> RouteConfiguration {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        route_match.headers_mut().push(header);

        let mut route_action = EnvoyRouteAction::new();
        route_action.set_cluster("cluster-1");

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);

        wrap_route("rc-1", route)
    }

    fn validate_header(header: EnvoyHeaderMatcher) -> HeaderMatchSpecifier {
        let validated = RouteConfigResource::validate(route_config_with_header(header))
            .expect("should validate");
        validated.virtual_hosts[0].routes[0].route_match.headers[0]
            .match_specifier
            .clone()
    }

    fn make_weighted_route(weights: &[(&str, Option<u32>)]) -> EnvoyRoute {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");

        let mut wc = EnvoyWeightedCluster::new();
        for (name, weight) in weights {
            let mut cw = ClusterWeight::new();
            cw.set_name(*name);
            if let Some(weight) = weight {
                let mut value = UInt32Value::new();
                value.set_value(*weight);
                cw.set_weight(value);
            }
            wc.clusters_mut().push(cw);
        }

        let mut route_action = EnvoyRouteAction::new();
        route_action.set_weighted_clusters(wc);

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);
        route
    }

    #[test]
    fn validate_basic() {
        let rc = make_route_config("rc-1");
        let validated = RouteConfigResource::validate(rc).expect("should validate");
        assert_eq!(validated.name, "rc-1");
        assert_eq!(validated.virtual_hosts.len(), 1);
        assert_eq!(validated.virtual_hosts[0].routes.len(), 1);
    }

    #[test]
    fn validate_empty_virtual_hosts() {
        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        let validated = RouteConfigResource::validate(rc).expect("should validate");
        assert!(validated.virtual_hosts.is_empty());
    }

    #[test]
    fn validate_virtual_host_no_domains() {
        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.routes_mut().push(make_route("/", "cluster-1"));

        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(vh);

        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("no domains"));
    }

    #[test]
    fn validate_virtual_host_rejects_invalid_domain_patterns() {
        for domain in ["", "a*b.example.com", "**", "*foo*", "foo**", "*foo*bar"] {
            let mut invalid = EnvoyVirtualHost::new();
            invalid.set_name("invalid-host");
            invalid.domains_mut().push(domain);
            let mut rc = make_route_config("rc-1");
            rc.virtual_hosts_mut().push(invalid);

            let err = RouteConfigResource::validate(rc).unwrap_err();
            assert!(
                err.to_string().contains("invalid domain pattern"),
                "{domain:?}: {err}"
            );
            assert!(
                err.to_string().contains("invalid-host"),
                "{domain:?}: {err}"
            );
        }
    }

    #[test]
    fn invalid_domain_patterns_fail_conversion() {
        for domain in ["", "a*b.example.com", "**", "*foo*", "foo**", "*foo*bar"] {
            assert_eq!(
                DomainMatcher::try_from(domain),
                Err(InvalidDomainPattern),
                "{domain:?}",
            );
        }
    }

    #[test]
    fn domain_patterns_convert_to_lowercase_matchers_without_wildcards() {
        for (domain, expected) in [
            ("example.com", DomainMatcher::Exact("example.com".into())),
            ("Example.COM", DomainMatcher::Exact("example.com".into())),
            ("*", DomainMatcher::Universal),
            (
                "*.Example.com",
                DomainMatcher::Suffix(".example.com".into()),
            ),
            ("*-foo", DomainMatcher::Suffix("-foo".into())),
            ("FOO.*", DomainMatcher::Prefix("foo.".into())),
            ("foo-*", DomainMatcher::Prefix("foo-".into())),
        ] {
            assert_eq!(DomainMatcher::try_from(domain), Ok(expected), "{domain:?}");
        }
    }

    #[test]
    fn validate_virtual_host_accepts_supported_domain_patterns() {
        let domains = [
            "api.example.com",
            "*.example.com",
            "*-bar.example.com",
            "api.*",
            "api-*",
            "api.example.com:443",
            "[2001:db8::1]:443",
        ];
        let mut host = EnvoyVirtualHost::new();
        host.set_name("host");
        for domain in domains {
            host.domains_mut().push(domain);
        }
        let mut rc = make_route_config("rc-1");
        rc.virtual_hosts_mut().push(host);

        let validated = RouteConfigResource::validate(rc).unwrap();
        assert_eq!(
            validated.virtual_hosts[1].domains,
            domains.map(|domain| DomainMatcher::try_from(domain).unwrap()),
        );
    }

    #[test]
    fn no_matching_virtual_host_does_not_invalidate_the_resource() {
        let mut host = EnvoyVirtualHost::new();
        host.set_name("host");
        host.domains_mut().push("other.example.com");
        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(host);

        let validated = RouteConfigResource::validate(rc).unwrap();
        assert_eq!(
            crate::routing::find_virtual_host_index("api.example.com", &validated.virtual_hosts),
            None,
        );
    }

    #[test]
    fn selected_virtual_host_limits_cluster_dependencies() {
        let mut rc = make_route_config("rc-1");
        let mut selected = EnvoyVirtualHost::new();
        selected.set_name("selected");
        selected.domains_mut().push("api.example.com");
        selected.routes_mut().push(make_weighted_route(&[
            ("cluster-a", Some(80)),
            ("cluster-b", Some(20)),
            ("cluster-unused", Some(0)),
        ]));
        rc.virtual_hosts_mut().push(selected);

        let validated = RouteConfigResource::validate(rc).unwrap();
        let index =
            crate::routing::find_virtual_host_index("API.EXAMPLE.COM", &validated.virtual_hosts)
                .unwrap();
        assert_eq!(
            validated.virtual_hosts[index].cluster_names(),
            HashSet::from(["cluster-a".to_string(), "cluster-b".to_string()]),
        );
    }

    #[test]
    fn validate_route_missing_match() {
        let mut route = EnvoyRoute::new();
        route.set_route(EnvoyRouteAction::new());

        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(vh);

        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("missing match field"));
    }

    #[test]
    fn validate_route_skips_query_parameter_matchers() {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        let mut qp = QueryParameterMatcher::new();
        qp.set_name("q");
        route_match.query_parameters_mut().push(qp);

        let mut route_action = EnvoyRouteAction::new();
        route_action.set_cluster("cluster-1");

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);

        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(vh);

        let validated = RouteConfigResource::validate(rc).expect("should validate");
        assert!(validated.virtual_hosts[0].routes.is_empty());
    }

    #[test]
    fn validate_route_rejects_non_route_action() {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_redirect(RedirectAction::new());

        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(vh);

        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("only route action is supported"));
    }

    #[test]
    fn validate_weighted_clusters() {
        let rc = wrap_route(
            "rc-1",
            make_weighted_route(&[("cluster-a", Some(80)), ("cluster-b", Some(20))]),
        );

        let validated = RouteConfigResource::validate(rc).expect("should validate");
        match &validated.virtual_hosts[0].routes[0].action {
            RouteAction::WeightedClusters(clusters) => {
                assert_eq!(clusters.len(), 2);
                assert_eq!(clusters[0].name, "cluster-a");
                assert_eq!(clusters[0].weight, 80);
                assert_eq!(clusters[1].name, "cluster-b");
                assert_eq!(clusters[1].weight, 20);
            }
            other => panic!("expected WeightedClusters, got {other:?}"),
        }
    }

    #[test]
    fn validate_weighted_clusters_drops_zero_weight_entries() {
        let rc = wrap_route(
            "rc-1",
            make_weighted_route(&[
                ("cluster-a", Some(80)),
                ("cluster-zero", Some(0)),
                ("cluster-unset", None),
            ]),
        );

        let validated = RouteConfigResource::validate(rc).expect("should validate");
        match &validated.virtual_hosts[0].routes[0].action {
            RouteAction::WeightedClusters(clusters) => {
                assert_eq!(clusters.len(), 1);
                assert_eq!(clusters[0].name, "cluster-a");
            }
            other => panic!("expected WeightedClusters, got {other:?}"),
        }
        // Zero-weight clusters never receive traffic, so they must not be subscribed to.
        assert_eq!(validated.virtual_hosts[0].cluster_names().len(), 1);
    }

    #[test]
    fn validate_weighted_clusters_rejects_zero_total_weight() {
        let rc = wrap_route(
            "rc-1",
            make_weighted_route(&[("cluster-a", Some(0)), ("cluster-b", None)]),
        );
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("non-zero weight"));
    }

    #[test]
    fn validate_weighted_clusters_rejects_overflowing_total_weight() {
        let rc = wrap_route(
            "rc-1",
            make_weighted_route(&[("cluster-a", Some(u32::MAX)), ("cluster-b", Some(1))]),
        );
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn validate_skips_route_with_unset_cluster_specifier() {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(EnvoyRouteAction::new());

        // An unset specifier -- which is also how an unknown specifier from a
        // newer control plane decodes -- must skip the route, not NACK.
        let validated =
            RouteConfigResource::validate(wrap_route("rc-1", route)).expect("should validate");
        assert!(validated.virtual_hosts[0].routes.is_empty());
    }

    #[test]
    fn validate_keeps_bin_header_matchers() {
        let mut header = EnvoyHeaderMatcher::new();
        header.set_name("x-authz-bin");
        header.set_present_match(true);

        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        route_match.headers_mut().push(header);

        let mut route_action = EnvoyRouteAction::new();
        route_action.set_cluster("cluster-1");

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);

        // Dropping the matcher would widen the route to every request; per A28 it
        // must be kept and evaluated as though the header were absent.
        let validated =
            RouteConfigResource::validate(wrap_route("rc-1", route)).expect("should validate");
        assert_eq!(
            validated.virtual_hosts[0].routes[0]
                .route_match
                .headers
                .len(),
            1
        );
    }

    #[test]
    #[should_panic = "virtual host domain should be valid UTF-8"]
    fn invalid_utf8_domain_panics() {
        let mut host = EnvoyVirtualHost::new();
        host.set_name("host");
        host.domains_mut()
            .push(protobuf::ProtoStr::from_utf8_unchecked(b"\xff"));
        let mut rc = make_route_config("rc-1");
        rc.virtual_hosts_mut().push(host);

        let _ = RouteConfigResource::validate(rc);
    }

    #[test]
    fn legacy_header_patterns_panic_on_invalid_utf8() {
        let value = protobuf::ProtoStr::from_utf8_unchecked(b"\xff");
        for (kind, field) in [
            ("exact", "exact header matcher"),
            ("prefix", "prefix header matcher"),
            ("suffix", "suffix header matcher"),
            ("contains", "contains header matcher"),
            ("regex", "header regex"),
        ] {
            let mut header = EnvoyHeaderMatcher::new();
            header.set_name("x-test");
            match kind {
                "exact" => header.set_exact_match(value),
                "prefix" => header.set_prefix_match(value),
                "suffix" => header.set_suffix_match(value),
                "contains" => header.set_contains_match(value),
                "regex" => {
                    let mut regex = RegexMatcher::new();
                    regex.set_regex(value);
                    header.set_safe_regex_match(regex);
                }
                _ => panic!("unknown matcher kind"),
            }
            let rc = route_config_with_header(header);
            let payload = catch_unwind(AssertUnwindSafe(|| RouteConfigResource::validate(rc)))
                .expect_err("expected a panic");
            let message = payload
                .downcast_ref::<String>()
                .expect("panic message should be a String");
            assert!(
                message.starts_with(&format!("{field} should be valid UTF-8:")),
                "{kind}: {message}"
            );
        }
    }

    #[test]
    fn validate_header_matcher_lowercases_name() {
        for (name, expected) in [
            ("X-Foo", "x-foo"),
            ("Content-Type", "content-type"),
            ("X-Trace-BIN", "x-trace-bin"),
            ("x-already-lower", "x-already-lower"),
        ] {
            let mut header = EnvoyHeaderMatcher::new();
            header.set_name(name);
            header.set_present_match(true);

            let validated = RouteConfigResource::validate(route_config_with_header(header))
                .expect("should validate");
            assert_eq!(
                validated.virtual_hosts[0].routes[0].route_match.headers[0].name,
                expected
            );
        }
    }

    #[test]
    fn validate_legacy_exact_header_matcher_preserves_empty_and_utf8_values() {
        for value in ["", "caf\u{e9}"] {
            let mut header = EnvoyHeaderMatcher::new();
            header.set_name("x-test");
            header.set_exact_match(value);

            let HeaderMatchSpecifier::String(matcher) = validate_header(header) else {
                panic!("expected a string matcher");
            };
            assert!(matcher.is_match(value));
            assert!(!matcher.is_match(&format!("{value}suffix")));
        }
    }

    #[test]
    fn validate_legacy_prefix_header_matcher() {
        let mut header = EnvoyHeaderMatcher::new();
        header.set_name("x-tenant");
        header.set_prefix_match("prod-");

        let HeaderMatchSpecifier::String(matcher) = validate_header(header) else {
            panic!("expected a string matcher");
        };
        assert!(matcher.is_match("prod-east"));
        assert!(!matcher.is_match("PROD-east"));
        assert!(!matcher.is_match("east-prod-"));
    }

    #[test]
    fn validate_legacy_suffix_header_matcher() {
        let mut header = EnvoyHeaderMatcher::new();
        header.set_name("x-tenant");
        header.set_suffix_match("-canary");

        let HeaderMatchSpecifier::String(matcher) = validate_header(header) else {
            panic!("expected a string matcher");
        };
        assert!(matcher.is_match("east-canary"));
        assert!(!matcher.is_match("east-CANARY"));
        assert!(!matcher.is_match("-canary-east"));
    }

    #[test]
    fn validate_legacy_contains_header_matcher() {
        let mut header = EnvoyHeaderMatcher::new();
        header.set_name("x-tenant");
        header.set_contains_match("staging");

        let HeaderMatchSpecifier::String(matcher) = validate_header(header) else {
            panic!("expected a string matcher");
        };
        assert!(matcher.is_match("east-staging-1"));
        assert!(!matcher.is_match("east-STAGING-1"));
        assert!(!matcher.is_match("east-stag"));
    }

    #[test]
    fn validate_legacy_string_header_matchers_reject_empty_values() {
        let mut prefix = EnvoyHeaderMatcher::new();
        prefix.set_name("x-tenant");
        prefix.set_prefix_match("");

        let mut suffix = EnvoyHeaderMatcher::new();
        suffix.set_name("x-tenant");
        suffix.set_suffix_match("");

        let mut contains = EnvoyHeaderMatcher::new();
        contains.set_name("x-tenant");
        contains.set_contains_match("");

        for (kind, header) in [
            ("prefix", prefix),
            ("suffix", suffix),
            ("contains", contains),
        ] {
            let err = RouteConfigResource::validate(route_config_with_header(header)).unwrap_err();
            assert!(err.to_string().contains(&format!("empty {kind} match")));
        }
    }

    #[test]
    fn validate_string_matcher_rejects_empty_prefix_suffix_and_contains() {
        let mut prefix = EnvoyStringMatcher::new();
        prefix.set_prefix("");
        let mut prefix_header = EnvoyHeaderMatcher::new();
        prefix_header.set_name("x-tenant");
        prefix_header.set_string_match(prefix);

        let mut suffix = EnvoyStringMatcher::new();
        suffix.set_suffix("");
        let mut suffix_header = EnvoyHeaderMatcher::new();
        suffix_header.set_name("x-tenant");
        suffix_header.set_string_match(suffix);

        let mut contains = EnvoyStringMatcher::new();
        contains.set_contains("");
        let mut contains_header = EnvoyHeaderMatcher::new();
        contains_header.set_name("x-tenant");
        contains_header.set_string_match(contains);

        for (kind, header) in [
            ("prefix", prefix_header),
            ("suffix", suffix_header),
            ("contains", contains_header),
        ] {
            let err = RouteConfigResource::validate(route_config_with_header(header)).unwrap_err();
            assert!(err.to_string().contains(&format!("empty {kind} match")));
        }
    }

    #[test]
    fn cluster_names_are_scoped_to_virtual_host() {
        let mut rc = make_route_config("rc-1");
        let mut other = EnvoyVirtualHost::new();
        other.set_name("vh2");
        other.domains_mut().push("other.example.com");
        other.routes_mut().push(make_route("/", "cluster-2"));
        rc.virtual_hosts_mut().push(other);

        let validated = RouteConfigResource::validate(rc).unwrap();
        assert_eq!(
            validated.virtual_hosts[0].cluster_names(),
            HashSet::from(["cluster-1".to_string()])
        );
        assert_eq!(
            validated.virtual_hosts[1].cluster_names(),
            HashSet::from(["cluster-2".to_string()])
        );
    }

    #[test]
    fn deserialize_roundtrip() {
        let rc = make_route_config("test");
        let bytes = rc.serialize().expect("serialize");
        let deserialized = RouteConfigResource::deserialize(bytes::Bytes::from(bytes)).unwrap();
        assert_eq!(RouteConfigResource::name(&deserialized), "test");
    }

    fn route_config_with_match(route_match: EnvoyRouteMatch) -> RouteConfiguration {
        let mut route_action = EnvoyRouteAction::new();
        route_action.set_cluster("cluster-1");

        let mut route = EnvoyRoute::new();
        route.set_match(route_match);
        route.set_route(route_action);

        let mut vh = EnvoyVirtualHost::new();
        vh.set_name("vh1");
        vh.domains_mut().push("*");
        vh.routes_mut().push(route);

        let mut rc = RouteConfiguration::new();
        rc.set_name("rc-1");
        rc.virtual_hosts_mut().push(vh);
        rc
    }

    #[test]
    fn validate_path_regex_errors_preserve_context() {
        for (pattern, reason) in [
            ("", "empty regex is not allowed"),
            ("[", "unclosed character class"),
        ] {
            let mut regex = RegexMatcher::new();
            regex.set_regex(pattern);
            let mut route_match = EnvoyRouteMatch::new();
            route_match.set_safe_regex(regex);

            let Error::Validation(message) =
                RouteConfigResource::validate(route_config_with_match(route_match)).unwrap_err()
            else {
                panic!("expected a validation error");
            };
            assert!(
                message.starts_with(&format!("invalid path regex '{pattern}':")),
                "{message}"
            );
            assert!(message.contains(reason), "{message}");
        }
    }

    #[test]
    fn validated_path_regex_requires_a_full_match() {
        let mut regex = RegexMatcher::new();
        regex.set_regex("/foo|/bar");
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_safe_regex(regex);

        let validated =
            RouteConfigResource::validate(route_config_with_match(route_match)).unwrap();
        let PathSpecifier::SafeRegex(regex) = &validated.virtual_hosts[0].routes[0]
            .route_match
            .path_specifier
        else {
            panic!("expected regex path matcher");
        };
        assert!(regex.is_match("/foo"));
        assert!(regex.is_match("/bar"));
        assert!(!regex.is_match("/foobar"));
        assert!(!regex.is_match("prefix/bar"));
    }

    #[test]
    fn validated_legacy_header_regex_requires_a_full_match() {
        let mut regex = RegexMatcher::new();
        regex.set_regex("foo|bar");
        let mut header = EnvoyHeaderMatcher::new();
        header.set_name("x-test");
        header.set_safe_regex_match(regex);

        let HeaderMatchSpecifier::String(matcher) = validate_header(header) else {
            panic!("expected string header matcher");
        };
        assert!(matcher.is_match("foo"));
        assert!(matcher.is_match("bar"));
        assert!(!matcher.is_match("foobar"));
        assert!(!matcher.is_match("xbar"));
    }

    #[test]
    fn validate_header_regex_errors_preserve_context() {
        for (pattern, reason) in [
            ("", "empty regex is not allowed"),
            ("[", "unclosed character class"),
        ] {
            let mut regex = RegexMatcher::new();
            regex.set_regex(pattern);
            let mut header = EnvoyHeaderMatcher::new();
            header.set_name("x-test");
            header.set_safe_regex_match(regex);

            let mut route_match = EnvoyRouteMatch::new();
            route_match.set_prefix("/");
            route_match.headers_mut().push(header);

            let Error::Validation(message) =
                RouteConfigResource::validate(route_config_with_match(route_match)).unwrap_err()
            else {
                panic!("expected a validation error");
            };
            assert!(
                message.starts_with(&format!("invalid header regex '{pattern}':")),
                "{message}"
            );
            assert!(message.contains(reason), "{message}");
        }
    }

    #[test]
    fn validate_rejects_empty_header_matcher_name() {
        let mut header = EnvoyHeaderMatcher::new();
        header.set_present_match(true);

        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        route_match.headers_mut().push(header);

        let err = RouteConfigResource::validate(route_config_with_match(route_match)).unwrap_err();
        assert!(err.to_string().contains("header matcher name is empty"));
    }

    #[test]
    fn validate_runtime_fraction_normalizes_denominator() {
        let mut frac = FractionalPercent::new();
        frac.set_numerator(25);
        frac.set_denominator(DenominatorType::Hundred);
        let mut rf = RuntimeFractionalPercent::new();
        rf.set_default_value(frac);

        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        route_match.set_runtime_fraction(rf);

        let validated =
            RouteConfigResource::validate(route_config_with_match(route_match)).unwrap();
        assert_eq!(
            validated.virtual_hosts[0].routes[0]
                .route_match
                .match_fraction,
            Some(250_000)
        );
    }

    #[test]
    fn validate_rejects_runtime_fraction_without_default_value() {
        let mut route_match = EnvoyRouteMatch::new();
        route_match.set_prefix("/");
        route_match.set_runtime_fraction(RuntimeFractionalPercent::new());

        let err = RouteConfigResource::validate(route_config_with_match(route_match)).unwrap_err();
        assert!(err.to_string().contains("default_value"));
    }
}
