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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use envoy_types::pb::envoy::config::core::v3::Metadata;
use envoy_types::pb::envoy::config::route::v3::{
    RetryPolicy, RouteConfiguration, RouteMatch, route, route_action, route_match,
};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use super::safe_regex::SafeRegex;
use super::string_matcher::StringMatcher;
/// A `typed_filter_metadata` entry — a `google.protobuf.Any` (a type URL plus an
/// encoded message value).
#[derive(Debug, Clone)]
pub struct TypedMetadata {
    /// Type URL identifying the message encoded in `value`.
    pub type_url: String,
    /// The encoded message; decode it according to `type_url`.
    pub value: Bytes,
}

/// Read-only view over an xDS resource's `metadata` (see
/// [`envoy.config.core.v3.Metadata`]).
///
/// Exposes both metadata maps — untyped `filter_metadata`
/// (`google.protobuf.Struct`) and typed `typed_filter_metadata`
/// (`google.protobuf.Any`) — as encoded bytes, so consumers can decode them with
/// their own prost messages.
///
/// This is the spec-native carrier for extensions that ride on standard xDS
/// `metadata`, surfaced so that a pre-route interceptor (or other consumer) can
/// read config attached to the `RouteConfiguration`.
///
/// [`envoy.config.core.v3.Metadata`]: https://www.envoyproxy.io/docs/envoy/latest/api-v3/config/core/v3/base.proto#envoy-v3-api-msg-config-core-v3-metadata
#[derive(Debug, Clone, Default)]
pub struct RouteConfigMetadata {
    /// Encoded `google.protobuf.Struct` bytes, keyed by `filter_metadata` namespace.
    filter_metadata: HashMap<String, Bytes>,
    /// Typed `google.protobuf.Any` entries, keyed by `typed_filter_metadata` namespace.
    typed_filter_metadata: HashMap<String, TypedMetadata>,
}

impl RouteConfigMetadata {
    /// Returns the encoded untyped `filter_metadata` `google.protobuf.Struct` for
    /// `namespace`, if present. Decode it with a prost message (for example a
    /// `Struct`, or a typed config proto sharing its wire format).
    #[must_use]
    pub fn filter_metadata(&self, namespace: &str) -> Option<Bytes> {
        self.filter_metadata.get(namespace).cloned()
    }

    /// Returns the typed `typed_filter_metadata` `google.protobuf.Any` for
    /// `namespace`, if present.
    #[must_use]
    pub fn typed_filter_metadata(&self, namespace: &str) -> Option<TypedMetadata> {
        self.typed_filter_metadata.get(namespace).cloned()
    }

    /// Returns `true` when both metadata maps are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filter_metadata.is_empty() && self.typed_filter_metadata.is_empty()
    }

    /// Constructs a `RouteConfigMetadata` directly from pre-encoded bytes.
    ///
    /// Lets downstream crates build realistic metadata to unit-test a
    /// [`PreRouteInterceptor`](crate::PreRouteInterceptor) without standing up
    /// a control plane.
    ///
    /// Takes already-encoded `google.protobuf.Struct` bytes so that this does
    /// not expose proto binding types to the public API surface.
    #[cfg(any(test, feature = "testutil"))]
    #[must_use]
    pub fn from_encoded(
        filter_metadata: HashMap<String, Bytes>,
        typed_filter_metadata: HashMap<String, TypedMetadata>,
    ) -> Self {
        Self {
            filter_metadata,
            typed_filter_metadata,
        }
    }

    /// Builds the view from an xDS `Metadata`, pre-encoding each namespace's
    /// `Struct`/`Any` to bytes.
    pub(crate) fn from_proto(metadata: Metadata) -> Self {
        let filter_metadata = metadata
            .filter_metadata
            .into_iter()
            .map(|(namespace, value)| (namespace, Bytes::from(value.encode_to_vec())))
            .collect();
        let typed_filter_metadata = metadata
            .typed_filter_metadata
            .into_iter()
            .map(|(namespace, any)| {
                (
                    namespace,
                    TypedMetadata {
                        type_url: any.type_url,
                        value: Bytes::from(any.value),
                    },
                )
            })
            .collect();
        Self {
            filter_metadata,
            typed_filter_metadata,
        }
    }
}

/// Validated RouteConfiguration.
#[derive(Debug, Clone, Default)]
pub(crate) struct RouteConfigResource {
    pub name: String,
    pub virtual_hosts: Vec<VirtualHostConfig>,
    /// Resource-level `metadata` (`filter_metadata`), surfaced for pre-route
    /// interceptors. Empty when the `RouteConfiguration` carried no metadata.
    pub metadata: RouteConfigMetadata,
}

/// Validated Envoy retry settings (gRFC A44) parsed from a `RouteAction` or
/// `VirtualHost` `retry_policy`. A transport-neutral resource-layer type.
#[derive(Debug, Clone)]
pub(crate) struct RouteRetryConfig {
    /// Envoy `retry_on` conditions, comma-separated (e.g. `"unavailable"`).
    pub retry_on: String,
    /// `num_retries` if set; `None` means the caller applies its own default.
    pub num_retries: Option<u32>,
    /// `retry_back_off.base_interval` if set.
    pub base_interval: Option<Duration>,
    /// `retry_back_off.max_interval` if set.
    pub max_interval: Option<Duration>,
}

impl RouteRetryConfig {
    /// Parse and validate an Envoy `RetryPolicy` (gRFC A44). Returns
    /// `Err(Validation)` — so the xDS client NACKs — when `num_retries < 1`, or
    /// when `retry_back_off` is set with a `base_interval` or `max_interval`
    /// that is not greater than zero.
    fn from_proto(rp: &RetryPolicy) -> xds_client::Result<Self> {
        let num_retries = match rp.num_retries.as_ref().map(|v| v.value) {
            Some(0) => {
                return Err(Error::Validation(
                    "retry_policy.num_retries must be >= 1".into(),
                ));
            }
            other => other,
        };

        let (base_interval, max_interval) = match rp.retry_back_off.as_ref() {
            Some(backoff) => {
                let base_interval = backoff
                    .base_interval
                    .as_ref()
                    .and_then(proto_duration)
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| {
                        Error::Validation(
                            "retry_policy.retry_back_off.base_interval must be greater than 0"
                                .into(),
                        )
                    })?;
                let max_interval = backoff
                    .max_interval
                    .as_ref()
                    .map(|m| {
                        proto_duration(m).filter(|d| !d.is_zero()).ok_or_else(|| {
                            Error::Validation(
                                "retry_policy.retry_back_off.max_interval must be greater than 0"
                                    .into(),
                            )
                        })
                    })
                    .transpose()?;
                (Some(base_interval), max_interval)
            }
            None => (None, None),
        };

        Ok(Self {
            retry_on: rp.retry_on.clone(),
            num_retries,
            base_interval,
            max_interval,
        })
    }
}

/// Maximum `seconds` for a well-formed `google.protobuf.Duration`.
const MAX_PROTO_DURATION_SECONDS: i64 = 315_576_000_000;

/// Convert a protobuf `Duration` to [`std::time::Duration`], returning `None` for
/// values outside the documented `google.protobuf.Duration` range. Rejected here
/// so an invalid retry policy fails validation rather than overflowing later.
fn proto_duration(d: &envoy_types::pb::google::protobuf::Duration) -> Option<Duration> {
    if !(0..=MAX_PROTO_DURATION_SECONDS).contains(&d.seconds)
        || !(0..=999_999_999).contains(&d.nanos)
    {
        return None;
    }
    let seconds = u64::try_from(d.seconds).ok()?;
    let nanos = u32::try_from(d.nanos).ok()?;
    Some(Duration::new(seconds, nanos))
}

/// Parse and validate an Envoy `RetryPolicy` into a shared [`RouteRetryConfig`].
fn parse_retry(rp: &RetryPolicy) -> xds_client::Result<Arc<RouteRetryConfig>> {
    Ok(Arc::new(RouteRetryConfig::from_proto(rp)?))
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
    /// Validated retry settings (gRFC A44): the route's own `RouteAction.retry_policy`,
    /// else the inherited `VirtualHost.retry_policy` (route-level fully overrides,
    /// no merge), else `None`. Routes that inherit share one `Arc`.
    pub retry_config: Option<Arc<RouteRetryConfig>>,
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
    SafeRegex(SafeRegex),
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
        let metadata = message
            .metadata
            .map(RouteConfigMetadata::from_proto)
            .unwrap_or_default();

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
            // gRFC A44: routes inherit the virtual host's retry policy unless they
            // set their own. Parse it once so inheriting routes share one `Arc`.
            let vh_retry = vh.retry_policy.as_ref().map(parse_retry).transpose()?;
            for route in vh.routes {
                if let Some(validated_route) = validate_route(route, vh_retry.as_ref())? {
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
            metadata,
        })
    }
}

/// Returns `Ok(None)` for routes that should be silently skipped (query param matchers,
/// unsupported cluster specifiers like `cluster_header`).
///
/// `vh_retry` is the virtual host's policy, inherited when the route sets none.
fn validate_route(
    route: envoy_types::pb::envoy::config::route::v3::Route,
    vh_retry: Option<&Arc<RouteRetryConfig>>,
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

    let validated_action;
    let route_retry = match action {
        route::Action::Route(mut route_action) => {
            // Take the retry policy before `route_action` is consumed; parse it
            // only if the route is kept, so dropped routes cost no parse.
            let retry_policy = route_action.retry_policy.take();
            match validate_route_action(route_action)? {
                Some(action) => validated_action = action,
                None => return Ok(None),
            }
            retry_policy.as_ref().map(parse_retry).transpose()?
        }
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
        retry_config: route_retry.or_else(|| vh_retry.cloned()),
    }))
}

fn validate_route_match(rm: RouteMatch) -> xds_client::Result<RouteConfigMatch> {
    use envoy_types::pb::envoy::r#type::v3::fractional_percent::DenominatorType;

    let path_specifier = match rm.path_specifier {
        Some(route_match::PathSpecifier::Prefix(p)) => PathSpecifierConfig::Prefix(p),
        Some(route_match::PathSpecifier::Path(p)) => PathSpecifierConfig::Path(p),
        Some(route_match::PathSpecifier::SafeRegex(r)) => {
            let re = SafeRegex::new(&r.regex)
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
            let re = SafeRegex::new(&r.regex).map_err(|e| {
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
    use envoy_types::pb::envoy::config::route::v3::{
        RetryPolicy, RouteAction, VirtualHost, retry_policy::RetryBackOff, route::Action,
        route_action::ClusterSpecifier,
    };
    use envoy_types::pb::google::protobuf::{Duration as ProtoDuration, UInt32Value};

    use super::*;

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

    fn retry_policy(retry_on: &str, num_retries: u32) -> RetryPolicy {
        RetryPolicy {
            retry_on: retry_on.to_string(),
            num_retries: Some(UInt32Value { value: num_retries }),
            ..Default::default()
        }
    }

    fn make_route_with_retry(
        prefix: &str,
        cluster: &str,
        retry: RetryPolicy,
    ) -> envoy_types::pb::envoy::config::route::v3::Route {
        envoy_types::pb::envoy::config::route::v3::Route {
            r#match: Some(RouteMatch {
                path_specifier: Some(route_match::PathSpecifier::Prefix(prefix.to_string())),
                ..Default::default()
            }),
            action: Some(Action::Route(RouteAction {
                cluster_specifier: Some(ClusterSpecifier::Cluster(cluster.to_string())),
                retry_policy: Some(retry),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn proto_dur(seconds: i64, nanos: i32) -> ProtoDuration {
        ProtoDuration { seconds, nanos }
    }

    fn retry_policy_with_backoff(
        base: Option<ProtoDuration>,
        max: Option<ProtoDuration>,
    ) -> RetryPolicy {
        RetryPolicy {
            retry_on: "unavailable".to_string(),
            retry_back_off: Some(RetryBackOff {
                base_interval: base,
                max_interval: max,
            }),
            ..Default::default()
        }
    }

    fn validate_with_route_retry(retry: RetryPolicy) -> xds_client::Result<RouteConfigResource> {
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![make_route_with_retry("/", "c1", retry)],
                ..Default::default()
            }],
            ..Default::default()
        };
        RouteConfigResource::validate(rc)
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
    fn safe_regex_path_matcher_requires_a_full_match() {
        use envoy_types::pb::envoy::r#type::matcher::v3::RegexMatcher;

        let unanchored_rm = RouteMatch {
            path_specifier: Some(route_match::PathSpecifier::SafeRegex(RegexMatcher {
                regex: r"/pkg\.Greeter/SayHello".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };

        let matched = validate_route_match(unanchored_rm).expect("valid regex");
        let PathSpecifierConfig::SafeRegex(re) = matched.path_specifier else {
            panic!("expected a SafeRegex path specifier");
        };

        assert!(
            re.is_match("/pkg.Greeter/SayHello"),
            "exact path must match"
        );
        assert!(
            !re.is_match("/pkg.Greeter/SayHelloAgain"),
            "a longer method sharing the prefix must not match"
        );
        assert!(
            !re.is_match("/other.Svc/x/pkg.Greeter/SayHello"),
            "the pattern must not match as a substring of a longer path"
        );
    }

    #[test]
    fn safe_regex_path_matcher_anchors_each_alternation_branch() {
        use envoy_types::pb::envoy::r#type::matcher::v3::RegexMatcher;

        let rm = RouteMatch {
            path_specifier: Some(route_match::PathSpecifier::SafeRegex(RegexMatcher {
                regex: "/a|/b".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };

        let matched = validate_route_match(rm).expect("valid regex");
        let PathSpecifierConfig::SafeRegex(re) = matched.path_specifier else {
            panic!("expected a SafeRegex path specifier");
        };

        assert!(re.is_match("/a"));
        assert!(re.is_match("/b"));
        assert!(
            !re.is_match("/aX"),
            "an alternation branch must not match a longer path"
        );
    }

    #[test]
    fn safe_regex_header_matcher_requires_a_full_match() {
        use envoy_types::pb::envoy::config::route::v3::HeaderMatcher;
        use envoy_types::pb::envoy::config::route::v3::header_matcher::HeaderMatchSpecifier;
        use envoy_types::pb::envoy::r#type::matcher::v3::RegexMatcher;

        #[allow(deprecated)]
        let hm = HeaderMatcher {
            name: "x-version".into(),
            header_match_specifier: Some(HeaderMatchSpecifier::SafeRegexMatch(RegexMatcher {
                regex: "v[0-9]+".into(),
                ..Default::default()
            })),
            ..Default::default()
        };

        let matched = validate_header_matcher(hm).expect("valid regex");
        let HeaderMatchSpecifierConfig::String(m) = matched.match_specifier else {
            panic!("expected a string matcher");
        };
        assert!(m.is_match("v2"));
        assert!(
            !m.is_match("v2-beta"),
            "a longer value sharing the prefix must not match"
        );
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

    #[test]
    fn test_vhost_retry_policy_inherited_and_route_overrides() {
        // gRFC A44: a route with no policy inherits the virtual host's; a route
        // with its own policy completely overrides it. Fields are private, so we
        // verify the *selection* structurally via `Arc` identity — value mapping
        // is covered by the `from_route_retry` tests in `client::retry`.
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                retry_policy: Some(retry_policy("unavailable", 3)),
                routes: vec![
                    make_route_with_retry("/own", "c-own", retry_policy("cancelled", 2)),
                    make_route("/inherit-a", "c-a"),
                    make_route("/inherit-b", "c-b"),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        let routes = &validated.virtual_hosts[0].routes;

        let own = routes[0].retry_config.as_ref().expect("route-level policy");
        let inherit_a = routes[1].retry_config.as_ref().expect("inherited policy");
        let inherit_b = routes[2].retry_config.as_ref().expect("inherited policy");

        // The overriding route gets its own config, distinct from the vhost's.
        assert!(!Arc::ptr_eq(own, inherit_a));
        // Inheriting routes share the single vhost-level config `Arc`.
        assert!(Arc::ptr_eq(inherit_a, inherit_b));
    }

    #[test]
    fn test_no_retry_policy_yields_none() {
        // No vhost policy and no route policy => the route carries no retry config.
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![make_route("/", "c1")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let validated = RouteConfigResource::validate(rc).unwrap();
        assert!(validated.virtual_hosts[0].routes[0].retry_config.is_none());
    }

    // gRFC A44: the resource must be NACKed (validation error) when num_retries < 1
    // or a set base_interval/max_interval is not greater than zero.

    #[test]
    fn test_retry_num_retries_zero_is_rejected() {
        let err = validate_with_route_retry(retry_policy("unavailable", 0)).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn test_retry_base_interval_zero_is_rejected() {
        let policy = retry_policy_with_backoff(Some(proto_dur(0, 0)), None);
        assert!(validate_with_route_retry(policy).is_err());
    }

    #[test]
    fn test_retry_base_interval_negative_is_rejected() {
        let policy = retry_policy_with_backoff(Some(proto_dur(-1, 0)), None);
        assert!(validate_with_route_retry(policy).is_err());
    }

    #[test]
    fn test_retry_back_off_without_base_interval_is_rejected() {
        // retry_back_off set but base_interval unset => base is 0 => rejected.
        let policy = retry_policy_with_backoff(None, Some(proto_dur(1, 0)));
        assert!(validate_with_route_retry(policy).is_err());
    }

    #[test]
    fn test_retry_max_interval_zero_is_rejected() {
        let policy =
            retry_policy_with_backoff(Some(proto_dur(0, 100_000_000)), Some(proto_dur(0, 0)));
        assert!(validate_with_route_retry(policy).is_err());
    }

    #[test]
    fn test_retry_base_interval_below_1ms_is_accepted() {
        // A44: values < 1ms are treated as 1ms (clamped), not rejected.
        let policy = retry_policy_with_backoff(Some(proto_dur(0, 500_000)), None);
        assert!(validate_with_route_retry(policy).is_ok());
    }

    #[test]
    fn test_retry_valid_backoff_is_accepted() {
        let mut policy =
            retry_policy_with_backoff(Some(proto_dur(0, 100_000_000)), Some(proto_dur(1, 0)));
        policy.num_retries = Some(UInt32Value { value: 2 });
        let validated = validate_with_route_retry(policy).expect("valid retry policy");
        assert!(validated.virtual_hosts[0].routes[0].retry_config.is_some());
    }

    #[test]
    fn test_vhost_retry_invalid_is_rejected() {
        // A virtual-host-level policy is validated the same way and NACKs on error.
        let rc = RouteConfiguration {
            name: "rc".to_string(),
            virtual_hosts: vec![VirtualHost {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                retry_policy: Some(retry_policy("unavailable", 0)),
                routes: vec![make_route("/", "c1")],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(RouteConfigResource::validate(rc).is_err());
    }
}
