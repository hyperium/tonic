//! Validated RouteConfiguration resource (RDS).

use std::collections::HashSet;

use bytes::Bytes;
use envoy_types::pb::envoy::config::route::v3::{
    RouteConfiguration, RouteMatch, route, route_action, route_match,
};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

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
}

/// Path matching specifier.
#[derive(Debug, Clone)]
pub(crate) enum PathSpecifierConfig {
    Prefix(String),
    Path(String),
    SafeRegex(String),
}

/// Header matching criteria.
#[derive(Debug, Clone)]
pub(crate) struct HeaderMatcherConfig {
    pub name: String,
    pub match_specifier: HeaderMatchSpecifierConfig,
    pub invert_match: bool,
}

/// Header match specifier variants.
#[derive(Debug, Clone)]
pub(crate) enum HeaderMatchSpecifierConfig {
    Exact(String),
    SafeRegex(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
    /// Match if header is present (any value).
    Present,
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
                let validated_route = validate_route(route)?;
                routes.push(validated_route);
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

fn validate_route(
    route: envoy_types::pb::envoy::config::route::v3::Route,
) -> xds_client::Result<RouteConfig> {
    let route_match = route
        .r#match
        .ok_or_else(|| Error::Validation("route missing match field".into()))?;

    let match_criteria = validate_route_match(route_match)?;

    let action = route
        .action
        .ok_or_else(|| Error::Validation("route missing action field".into()))?;

    let validated_action = match action {
        route::Action::Route(route_action) => validate_route_action(route_action)?,
        route::Action::NonForwardingAction(_) => {
            // Non-forwarding actions are used in xDS server-side, not client-side.
            return Err(Error::Validation(
                "non_forwarding_action not supported for client-side routing".into(),
            ));
        }
        _ => {
            return Err(Error::Validation(
                "only route action is supported for client routing".into(),
            ));
        }
    };

    Ok(RouteConfig {
        match_criteria,
        action: validated_action,
    })
}

fn validate_route_match(rm: RouteMatch) -> xds_client::Result<RouteConfigMatch> {
    let path_specifier = match rm.path_specifier {
        Some(route_match::PathSpecifier::Prefix(p)) => PathSpecifierConfig::Prefix(p),
        Some(route_match::PathSpecifier::Path(p)) => PathSpecifierConfig::Path(p),
        Some(route_match::PathSpecifier::SafeRegex(r)) => PathSpecifierConfig::SafeRegex(r.regex),
        None => {
            // Default: empty prefix matches everything.
            PathSpecifierConfig::Prefix(String::new())
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
        let validated_hm = validate_header_matcher(hm)?;
        headers.push(validated_hm);
    }

    Ok(RouteConfigMatch {
        path_specifier,
        headers,
        case_sensitive,
    })
}

fn validate_header_matcher(
    hm: envoy_types::pb::envoy::config::route::v3::HeaderMatcher,
) -> xds_client::Result<HeaderMatcherConfig> {
    use envoy_types::pb::envoy::config::route::v3::header_matcher::HeaderMatchSpecifier;
    use envoy_types::pb::envoy::r#type::matcher::v3::string_matcher::MatchPattern;

    let match_specifier = match hm.header_match_specifier {
        Some(HeaderMatchSpecifier::ExactMatch(v)) => HeaderMatchSpecifierConfig::Exact(v),
        Some(HeaderMatchSpecifier::SafeRegexMatch(r)) => {
            HeaderMatchSpecifierConfig::SafeRegex(r.regex)
        }
        Some(HeaderMatchSpecifier::PresentMatch(_)) => HeaderMatchSpecifierConfig::Present,
        Some(HeaderMatchSpecifier::StringMatch(sm)) => match sm.match_pattern {
            Some(MatchPattern::Exact(v)) => HeaderMatchSpecifierConfig::Exact(v),
            Some(MatchPattern::Prefix(v)) => HeaderMatchSpecifierConfig::Prefix(v),
            Some(MatchPattern::Suffix(v)) => HeaderMatchSpecifierConfig::Suffix(v),
            Some(MatchPattern::Contains(v)) => HeaderMatchSpecifierConfig::Contains(v),
            Some(MatchPattern::SafeRegex(r)) => HeaderMatchSpecifierConfig::SafeRegex(r.regex),
            _ => {
                return Err(Error::Validation(
                    "unsupported StringMatcher pattern".into(),
                ));
            }
        },
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

fn validate_route_action(
    ra: envoy_types::pb::envoy::config::route::v3::RouteAction,
) -> xds_client::Result<RouteConfigAction> {
    match ra.cluster_specifier {
        Some(route_action::ClusterSpecifier::Cluster(name)) => {
            if name.is_empty() {
                return Err(Error::Validation("cluster name is empty".into()));
            }
            Ok(RouteConfigAction::Cluster(name))
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
            Ok(RouteConfigAction::WeightedClusters(clusters))
        }
        Some(_) => Err(Error::Validation(
            "unsupported cluster specifier variant".into(),
        )),
        None => Err(Error::Validation(
            "route action missing cluster specifier".into(),
        )),
    }
}

impl RouteConfigResource {
    /// Returns cluster names referenced by this route configuration for cascading CDS subscriptions.
    pub(crate) fn cascade_cluster_names(&self) -> HashSet<String> {
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
    fn test_cascade_cluster_names() {
        let rc = make_route_config("rc-1");
        let validated = RouteConfigResource::validate(rc).unwrap();
        let clusters = validated.cascade_cluster_names();
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
            validated.virtual_hosts[0].routes[0]
                .match_criteria
                .path_specifier,
            PathSpecifierConfig::Path(_)
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
        let clusters = validated.cascade_cluster_names();
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
}
