//! Validated RouteConfiguration resource (RDS).

use std::collections::HashSet;

use bytes::Bytes;
use envoy_types::pb::envoy::config::route::v3::{
    RouteConfiguration, RouteMatch, route, route_action, route_match,
};
use prost::Message;
use regex::Regex;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use super::string_matcher::StringMatcher;

/// Validated RouteConfiguration.
#[derive(Debug, Clone)]
pub(crate) struct RouteConfigResource {
    pub name: String,
    pub virtual_hosts: Vec<VirtualHostConfig>,
}

/// Validated virtual host with domain matching and routes.
#[derive(Debug, Clone)]
pub(crate) struct VirtualHostConfig {
    pub name: String,
    pub domains: Vec<String>,
    pub routes: Vec<RouteConfig>,
}

/// A validated route with match criteria and action.
#[derive(Debug, Clone)]
pub(crate) struct RouteConfig {
    pub match_criteria: RouteConfigMatch,
    pub action: RouteConfigAction,
}

/// Validated route match criteria.
#[derive(Debug, Clone)]
pub(crate) struct RouteConfigMatch {
    pub path_specifier: PathSpecifierConfig,
    pub headers: Vec<HeaderMatcherConfig>,
    pub case_sensitive: bool,
    /// Fraction of requests this route should match, as numerator out of 1,000,000.
    /// `None` means always match (100%).
    pub match_fraction: Option<u32>,
}

/// Path matching specifier.
#[derive(Debug, Clone)]
pub(crate) enum PathSpecifierConfig {
    Prefix(String),
    Path(String),
    SafeRegex(Regex),
}

/// Header matching criteria.
#[derive(Debug, Clone)]
pub(crate) struct HeaderMatcherConfig {
    pub name: String,
    pub match_specifier: HeaderMatchSpecifierConfig,
    pub invert_match: bool,
}

/// Header match specifier variants.
///
/// The `String` variant carries a generic [`StringMatcher`] (exact / prefix /
/// suffix / contains / safe_regex, with optional ASCII case-insensitive
/// matching per gRFC A63). `Present`, `Absent`, and `Range` are header-specific
/// extensions beyond the generic StringMatcher.
#[derive(Debug, Clone)]
pub(crate) enum HeaderMatchSpecifierConfig {
    String(StringMatcher),
    /// Match if header is present (any value).
    Present,
    /// Match if header is absent.
    Absent,
    /// Match if the header value, parsed as an integer, falls within [start, end).
    Range {
        start: i64,
        end: i64,
    },
}

/// Route action deciding where to send traffic.
#[derive(Debug, Clone)]
pub(crate) enum RouteConfigAction {
    Cluster(String),
    WeightedClusters(Vec<WeightedCluster>),
}

/// A cluster with an associated weight for traffic splitting.
#[derive(Debug, Clone)]
pub(crate) struct WeightedCluster {
    pub name: String,
    pub weight: u32,
}

impl Resource for RouteConfigResource {
    type Message = RouteConfiguration;

    const TYPE_URL: TypeUrl =
        TypeUrl::new("type.googleapis.com/envoy.config.route.v3.RouteConfiguration");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

    fn deserialize(bytes: Bytes) -> xds_client::Result<Self::Message> {
        RouteConfiguration::decode(bytes).map_err(Into::into)
    }

    fn name(message: &Self::Message) -> &str {
        &message.name
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name;

        if message.virtual_hosts.is_empty() {
            return Err(Error::Validation(format!(
                "route configuration '{name}' has no virtual hosts"
            )));
        }

        let mut virtual_hosts = Vec::with_capacity(message.virtual_hosts.len());

        for vh in message.virtual_hosts {
            if vh.domains.is_empty() {
                return Err(Error::Validation(format!(
                    "virtual host '{}' has no domains",
                    vh.name
                )));
            }

            let mut routes = Vec::with_capacity(vh.routes.len());
            for route in vh.routes {
                if let Some(validated_route) = validate_route(route)? {
                    routes.push(validated_route);
                }
            }

            virtual_hosts.push(VirtualHostConfig {
                name: vh.name,
                domains: vh.domains,
                routes,
            });
        }

        Ok(RouteConfigResource {
            name,
            virtual_hosts,
        })
    }
}

/// Returns `Ok(None)` for routes that should be silently skipped (query param matchers,
/// unsupported cluster specifiers like `cluster_header`).
fn validate_route(
    route: envoy_types::pb::envoy::config::route::v3::Route,
) -> xds_client::Result<Option<RouteConfig>> {
    let route_match = route
        .r#match
        .ok_or_else(|| Error::Validation("route missing match field".into()))?;

    // Per A28: ignore routes with query parameter matchers.
    if !route_match.query_parameters.is_empty() {
        return Ok(None);
    }

    let match_criteria = validate_route_match(route_match)?;

    let action = route
        .action
        .ok_or_else(|| Error::Validation("route missing action field".into()))?;

    let validated_action = match action {
        route::Action::Route(route_action) => match validate_route_action(route_action)? {
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

    Ok(Some(RouteConfig {
        match_criteria,
        action: validated_action,
    }))
}

fn validate_route_match(rm: RouteMatch) -> xds_client::Result<RouteConfigMatch> {
    use envoy_types::pb::envoy::r#type::v3::fractional_percent::DenominatorType;

    let path_specifier = match rm.path_specifier {
        Some(route_match::PathSpecifier::Prefix(p)) => PathSpecifierConfig::Prefix(p),
        Some(route_match::PathSpecifier::Path(p)) => PathSpecifierConfig::Path(p),
        Some(route_match::PathSpecifier::SafeRegex(r)) => {
            let re = Regex::new(&r.regex)
                .map_err(|e| Error::Validation(format!("invalid path regex '{}': {e}", r.regex)))?;
            PathSpecifierConfig::SafeRegex(re)
        }
        // Per A28: not having path_specifier will cause a NACK.
        None => {
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

    let case_sensitive = rm.case_sensitive.map(|v| v.value).unwrap_or(true);

    let mut headers = Vec::with_capacity(rm.headers.len());
    for hm in rm.headers {
        // Per A28: exclude headers with -bin suffix from matching.
        if hm.name.ends_with("-bin") {
            continue;
        }
        let validated_hm = validate_header_matcher(hm)?;
        headers.push(validated_hm);
    }

    // Per A28: use runtime_fraction.default_value, normalize to numerator out of 1,000,000.
    // runtime_key is ignored (gRPC has no runtime config).
    let match_fraction = rm
        .runtime_fraction
        .and_then(|rf| rf.default_value)
        .map(|frac| {
            let scale = match DenominatorType::try_from(frac.denominator) {
                Ok(DenominatorType::Hundred) => 10_000,
                Ok(DenominatorType::TenThousand) => 100,
                Ok(DenominatorType::Million) => 1,
                Err(_) => 1,
            };
            (frac.numerator.saturating_mul(scale)).min(1_000_000)
        });

    Ok(RouteConfigMatch {
        path_specifier,
        headers,
        case_sensitive,
        match_fraction,
    })
}

fn validate_header_matcher(
    hm: envoy_types::pb::envoy::config::route::v3::HeaderMatcher,
) -> xds_client::Result<HeaderMatcherConfig> {
    use envoy_types::pb::envoy::config::route::v3::header_matcher::HeaderMatchSpecifier;

    // It's common that some xDS features are marked as deprecated while they are still widely in-use.
    #[allow(deprecated)]
    let match_specifier = match hm.header_match_specifier {
        // TODO: Remove this arm once ExactMatch is fully removed from envoy-types.
        // ExactMatch is deprecated in favor of StringMatch, which is handled below.
        #[allow(deprecated)]
        Some(HeaderMatchSpecifier::ExactMatch(v)) => {
            HeaderMatchSpecifierConfig::String(StringMatcher::Exact {
                value: v,
                ignore_case: false,
            })
        }
        // TODO: Remove this arm once SafeRegexMatch is fully removed from envoy-types.
        // SafeRegexMatch is deprecated in favor of StringMatch, which is handled below.
        #[allow(deprecated)]
        Some(HeaderMatchSpecifier::SafeRegexMatch(r)) => {
            let re = Regex::new(&r.regex).map_err(|e| {
                Error::Validation(format!("invalid header regex '{}': {e}", r.regex))
            })?;
            HeaderMatchSpecifierConfig::String(StringMatcher::SafeRegex(re))
        }
        Some(HeaderMatchSpecifier::RangeMatch(r)) => HeaderMatchSpecifierConfig::Range {
            start: r.start,
            end: r.end,
        },
        Some(HeaderMatchSpecifier::PresentMatch(present)) => {
            if present {
                HeaderMatchSpecifierConfig::Present
            } else {
                HeaderMatchSpecifierConfig::Absent
            }
        }
        Some(HeaderMatchSpecifier::StringMatch(sm)) => {
            HeaderMatchSpecifierConfig::String(StringMatcher::from_proto(sm)?)
        }
        None => HeaderMatchSpecifierConfig::Present,
        _ => {
            return Err(Error::Validation(
                "unsupported header match specifier".into(),
            ));
        }
    };

    Ok(HeaderMatcherConfig {
        name: hm.name,
        match_specifier,
        invert_match: hm.invert_match,
    })
}

/// Returns `Ok(None)` for routes with unsupported cluster specifiers (e.g. `cluster_header`).
fn validate_route_action(
    ra: envoy_types::pb::envoy::config::route::v3::RouteAction,
) -> xds_client::Result<Option<RouteConfigAction>> {
    match ra.cluster_specifier {
        Some(route_action::ClusterSpecifier::Cluster(name)) => {
            if name.is_empty() {
                return Err(Error::Validation("cluster name is empty".into()));
            }
            Ok(Some(RouteConfigAction::Cluster(name)))
        }
        Some(route_action::ClusterSpecifier::WeightedClusters(wc)) => {
            if wc.clusters.is_empty() {
                return Err(Error::Validation("weighted_clusters is empty".into()));
            }
            let clusters: Vec<WeightedCluster> = wc
                .clusters
                .into_iter()
                .map(|c| WeightedCluster {
                    name: c.name,
                    weight: c.weight.map(|w| w.value).unwrap_or(0),
                })
                .collect();
            Ok(Some(RouteConfigAction::WeightedClusters(clusters)))
        }
        // Per A28: silently ignore routes with cluster_header or other unsupported specifiers.
        Some(_) => Ok(None),
        None => Err(Error::Validation(
            "route action missing cluster specifier".into(),
        )),
    }
}

impl RouteConfigResource {
    /// Returns cluster names referenced by this route configuration for cascading CDS subscriptions.
    pub(crate) fn cluster_names(&self) -> HashSet<String> {
        let mut clusters = HashSet::new();
        for vh in &self.virtual_hosts {
            for route in &vh.routes {
                match &route.action {
                    RouteConfigAction::Cluster(name) => {
                        clusters.insert(name.clone());
                    }
                    RouteConfigAction::WeightedClusters(wcs) => {
                        for wc in wcs {
                            clusters.insert(wc.name.clone());
                        }
                    }
                }
            }
        }
        clusters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::config::route::v3::{
        RouteAction, VirtualHost, route::Action, route_action::ClusterSpecifier,
    };

    fn make_route(prefix: &str, cluster: &str) -> envoy_types::pb::envoy::config::route::v3::Route {
        envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix(prefix.to_string())),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster(cluster.to_string())),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn make_route_config(name: &str) -> RouteConfiguration {
        RouteConfiguration {
            name: name.to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![make_route("/", "cluster-1")],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_basic() {
        let rc = make_route_config("rc-1");
        let validated = RouteConfigResource::validate(rc).expect("should validate");
        assert_eq!(validated.name, "rc-1");
        assert_eq!(validated.virtual_hosts.len(), 1);
        assert_eq!(validated.virtual_hosts[0].routes.len(), 1);
    }

    #[test]
    fn test_cluster_names() {
        let rc = make_route_config("rc-1");
        let validated = RouteConfigResource::validate(rc).unwrap();
        let clusters = validated.cluster_names();
        assert_eq!(clusters.len(), 1);
        assert!(clusters.contains("cluster-1"));
    }

    #[test]
    fn test_validate_empty_domains() {
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh-no-domains".to_string(),
                domains: vec![],
                routes: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("no domains"));
    }

    #[test]
    fn test_validate_empty_cluster_name() {
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![make_route("/", "")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("cluster name is empty"));
    }

    #[test]
    fn test_validate_exact_path() {
        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Path(
                    "/service/Method".to_string(),
                )),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        assert!(matches!(
            &validated.virtual_hosts[0].routes[0]
                .match_criteria
                .path_specifier,
            PathSpecifierConfig::Path(p) if p == "/service/Method"
        ));
    }

    #[test]
    fn test_cascade_weighted_clusters() {
        use envoy_types::pb::envoy::config::route::v3::{
            WeightedCluster, weighted_cluster::ClusterWeight,
        };
        use envoy_types::pb::google::protobuf::UInt32Value;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(route_action::ClusterSpecifier::WeightedClusters(
                    WeightedCluster {
                        clusters: vec![
                            ClusterWeight {
                                name: "c1".to_string(),
                                weight: Some(UInt32Value { value: 70 }),
                                ..Default::default()
                            },
                            ClusterWeight {
                                name: "c2".to_string(),
                                weight: Some(UInt32Value { value: 30 }),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        let clusters = validated.cluster_names();
        assert_eq!(clusters.len(), 2);
        assert!(clusters.contains("c1"));
        assert!(clusters.contains("c2"));
    }

    #[test]
    fn test_not_all_resources_required() {
        assert!(!RouteConfigResource::ALL_RESOURCES_REQUIRED_IN_SOTW);
    }

    #[test]
    fn test_deserialize_roundtrip() {
        let rc = make_route_config("rc-1");
        let bytes = rc.encode_to_vec();
        let deserialized = RouteConfigResource::deserialize(Bytes::from(bytes)).unwrap();
        assert_eq!(RouteConfigResource::name(&deserialized), "rc-1");
    }

    #[test]
    fn test_invalid_regex_fails_validation() {
        use envoy_types::pb::envoy::config::route::v3::{
            RouteAction, VirtualHost, route::Action, route_action::ClusterSpecifier,
        };
        use envoy_types::pb::envoy::r#type::matcher::v3::RegexMatcher;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::SafeRegex(RegexMatcher {
                    regex: "[invalid".to_string(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("invalid path regex"));
    }

    #[test]
    fn test_empty_virtual_hosts_fails() {
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![],
            ..Default::default()
        };
        let err = RouteConfigResource::validate(rc).unwrap_err();
        assert!(err.to_string().contains("no virtual hosts"));
    }

    #[test]
    fn test_route_with_query_params_is_skipped() {
        use envoy_types::pb::envoy::config::route::v3::QueryParameterMatcher;

        let route_with_qp = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                query_parameters: vec![QueryParameterMatcher {
                    name: "key".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route_with_qp, make_route("/", "c2")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        // Only the second route (without query params) should remain.
        assert_eq!(validated.virtual_hosts[0].routes.len(), 1);
        assert!(matches!(
            &validated.virtual_hosts[0].routes[0].action,
            RouteConfigAction::Cluster(c) if c == "c2"
        ));
    }

    #[test]
    fn test_route_with_cluster_header_is_skipped() {
        let route_with_ch = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(route_action::ClusterSpecifier::ClusterHeader(
                    "x-cluster".to_string(),
                )),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route_with_ch, make_route("/", "c1")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        assert_eq!(validated.virtual_hosts[0].routes.len(), 1);
        assert!(matches!(
            &validated.virtual_hosts[0].routes[0].action,
            RouteConfigAction::Cluster(c) if c == "c1"
        ));
    }

    #[test]
    fn test_match_fraction_normalized_to_million() {
        use envoy_types::pb::envoy::config::core::v3::RuntimeFractionalPercent;
        use envoy_types::pb::envoy::r#type::v3::FractionalPercent;
        use envoy_types::pb::envoy::r#type::v3::fractional_percent::DenominatorType;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                runtime_fraction: Some(RuntimeFractionalPercent {
                    default_value: Some(FractionalPercent {
                        numerator: 50,
                        denominator: DenominatorType::Hundred as i32,
                    }),
                    runtime_key: String::new(),
                }),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        // 50/100 = 500,000/1,000,000
        assert_eq!(
            validated.virtual_hosts[0].routes[0]
                .match_criteria
                .match_fraction,
            Some(500_000)
        );
    }

    #[test]
    fn test_match_fraction_capped_at_million() {
        use envoy_types::pb::envoy::config::core::v3::RuntimeFractionalPercent;
        use envoy_types::pb::envoy::r#type::v3::FractionalPercent;
        use envoy_types::pb::envoy::r#type::v3::fractional_percent::DenominatorType;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                runtime_fraction: Some(RuntimeFractionalPercent {
                    default_value: Some(FractionalPercent {
                        numerator: 200,
                        denominator: DenominatorType::Hundred as i32,
                    }),
                    runtime_key: String::new(),
                }),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        assert_eq!(
            validated.virtual_hosts[0].routes[0]
                .match_criteria
                .match_fraction,
            Some(1_000_000)
        );
    }

    #[test]
    fn test_range_match_header() {
        use envoy_types::pb::envoy::config::route::v3::HeaderMatcher;
        use envoy_types::pb::envoy::config::route::v3::header_matcher::HeaderMatchSpecifier;
        use envoy_types::pb::envoy::r#type::v3::Int64Range;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                headers: vec![HeaderMatcher {
                    name: "x-version".to_string(),
                    header_match_specifier: Some(HeaderMatchSpecifier::RangeMatch(Int64Range {
                        start: 1,
                        end: 10,
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        assert!(matches!(
            &validated.virtual_hosts[0].routes[0].match_criteria.headers[0].match_specifier,
            HeaderMatchSpecifierConfig::Range { start: 1, end: 10 }
        ));
    }

    #[test]
    fn test_binary_header_excluded_at_validation() {
        use envoy_types::pb::envoy::config::route::v3::HeaderMatcher;
        use envoy_types::pb::envoy::config::route::v3::header_matcher::HeaderMatchSpecifier;
        use envoy_types::pb::envoy::r#type::matcher::v3::StringMatcher;
        use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;

        let route = envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix("/".to_string())),
                headers: vec![
                    HeaderMatcher {
                        name: "x-data-bin".to_string(),
                        header_match_specifier: Some(HeaderMatchSpecifier::StringMatch(
                            StringMatcher {
                                match_pattern: Some(MatchPattern::Exact("secret".to_string())),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    },
                    HeaderMatcher {
                        name: "x-env".to_string(),
                        header_match_specifier: Some(HeaderMatchSpecifier::StringMatch(
                            StringMatcher {
                                match_pattern: Some(MatchPattern::Exact("prod".to_string())),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster("c1".to_string())),
                ..Default::default()
            })),
            ..Default::default()
        };
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![route],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        let headers = &validated.virtual_hosts[0].routes[0].match_criteria.headers;
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name, "x-env");
    }
}
