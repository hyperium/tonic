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

//! Validated Cluster resource (CDS).

use protobuf::Parse;
use xds_client::Error;
use xds_client::Resource;
use xds_client::resource::TypeUrl;

use crate::generated::envoy::config::cluster::v3::Cluster;
use crate::generated::envoy::config::cluster::v3::cluster::ClusterDiscoveryTypeOneof;
use crate::generated::envoy::config::cluster::v3::cluster::DiscoveryType;
use crate::generated::envoy::extensions::clusters::aggregate::v3::ClusterConfig as AggregateClusterConfig;

/// Extension name and `typed_config` type for gRFC A37 aggregate clusters.
const AGGREGATE_CLUSTER_NAME: &str = "envoy.clusters.aggregate";
const AGGREGATE_CLUSTER_CONFIG_TYPE_URL: &str =
    "type.googleapis.com/envoy.extensions.clusters.aggregate.v3.ClusterConfig";

// TODO: model `transport_socket` (security) and load-balancing-policy
// config once the dependency manager and LB policies exist to consume them.
/// Validated Cluster resource.
///
/// Only the discovery mechanism is modeled for now (gRFC A27/A37).
#[derive(Debug, Clone)]
pub(crate) struct ClusterResource {
    pub(crate) name: String,
    pub(crate) discovery: ClusterDiscovery,
}

/// How a cluster's endpoints are discovered, mirroring the
/// `Cluster.cluster_discovery_type` oneof plus the `envoy.clusters.aggregate`
/// custom cluster type extension (gRFC A37).
///
/// `STATIC`, `STRICT_DNS`, and `ORIGINAL_DST` are not supported: they have no
/// gRPC xDS use case.
#[derive(Debug, Clone)]
pub(crate) enum ClusterDiscovery {
    /// Endpoints are discovered via EDS. When CDS resource left `eds_service_name` unset,
    /// it is resolved to the cluster name.
    Eds { eds_service_name: String },
    /// Endpoints are discovered via DNS resolution of a single target.
    LogicalDns { hostname: String, port: u16 },
    /// This is an aggregate cluster (gRFC A37): traffic falls over across
    /// `children` in priority order. Each child is itself a top-level cluster
    /// resolved independently (see `crate::xds_config::XdsConfig::clusters`).
    Aggregate { children: Vec<String> },
}

impl Resource for ClusterResource {
    type Message = Cluster;

    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.cluster.v3.Cluster");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

    fn deserialize(bytes: bytes::Bytes) -> xds_client::Result<Self::Message> {
        Cluster::parse(&bytes)
            .map_err(|e| Error::Validation(format!("failed to decode Cluster: {e}")))
    }

    fn name(message: &Self::Message) -> &str {
        message.name().to_str().unwrap_or_default()
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name().to_str().unwrap_or_default().to_string();
        if name.is_empty() {
            return Err(Error::Validation("cluster name is empty".into()));
        }

        let discovery = match message.cluster_discovery_type() {
            ClusterDiscoveryTypeOneof::Type(DiscoveryType::Eds) => {
                validate_eds_discovery(&message, &name)?
            }
            ClusterDiscoveryTypeOneof::Type(DiscoveryType::LogicalDns) => {
                validate_logical_dns_discovery(&message)?
            }
            ClusterDiscoveryTypeOneof::ClusterType(custom)
                if custom.name().to_str().unwrap_or_default() == AGGREGATE_CLUSTER_NAME =>
            {
                validate_aggregate_discovery(custom)?
            }
            other => {
                return Err(Error::Validation(format!(
                    "unsupported cluster discovery type: {other:?}"
                )));
            }
        };

        Ok(ClusterResource { name, discovery })
    }
}

fn validate_eds_discovery(
    message: &Cluster,
    cluster_name: &str,
) -> xds_client::Result<ClusterDiscovery> {
    let eds_cluster_config = message.eds_cluster_config();

    // Per gRFC A27: eds_config must be set, and must point at ADS or Self.
    if !eds_cluster_config.has_eds_config() {
        return Err(Error::Validation(
            "CDS's EDS config source is not set".into(),
        ));
    }
    let config_source = eds_cluster_config.eds_config();
    if !config_source.has_ads() && !config_source.has_self() {
        return Err(Error::Validation(
            "CDS's EDS config source is not ADS or Self".into(),
        ));
    }

    let eds_service_name = eds_cluster_config
        .service_name()
        .to_str()
        .unwrap_or_default();
    let eds_service_name = if eds_service_name.is_empty() {
        // Per gRFC A47, `xdstp:`-scheme (federation) cluster names must set an
        // explicit EDS service name rather than relying on the cluster-name fallback.
        if cluster_name.starts_with("xdstp:") {
            return Err(Error::Validation(
                "CDS's EDS service name is not set with a new-style cluster name".into(),
            ));
        }
        cluster_name.to_string()
    } else {
        eds_service_name.to_string()
    };

    Ok(ClusterDiscovery::Eds { eds_service_name })
}

fn validate_logical_dns_discovery(message: &Cluster) -> xds_client::Result<ClusterDiscovery> {
    if !message.has_load_assignment() {
        return Err(Error::Validation(
            "load_assignment not present for LOGICAL_DNS cluster".into(),
        ));
    }
    let load_assignment = message.load_assignment();

    let localities = load_assignment.endpoints();
    if localities.len() != 1 {
        return Err(Error::Validation(format!(
            "load_assignment for LOGICAL_DNS cluster must have exactly one locality, got {}",
            localities.len()
        )));
    }
    let lb_endpoints = localities.get(0).expect("checked len == 1").lb_endpoints();
    if lb_endpoints.len() != 1 {
        return Err(Error::Validation(format!(
            "locality for LOGICAL_DNS cluster must have exactly one endpoint, got {}",
            lb_endpoints.len()
        )));
    }
    let lb_endpoint = lb_endpoints.get(0).expect("checked len == 1");

    if !lb_endpoint.has_endpoint() {
        return Err(Error::Validation(
            "endpoint for LOGICAL_DNS cluster not set".into(),
        ));
    }
    let endpoint = lb_endpoint.endpoint();

    if !endpoint.has_address() {
        return Err(Error::Validation(
            "socket address for endpoint for LOGICAL_DNS cluster not set".into(),
        ));
    }
    let address = endpoint.address();
    if !address.has_socket_address() {
        return Err(Error::Validation(
            "socket address for endpoint for LOGICAL_DNS cluster not set".into(),
        ));
    }
    let socket_address = address.socket_address();

    let resolver_name = socket_address.resolver_name();
    if !resolver_name.is_empty() {
        return Err(Error::Validation(format!(
            "socket address for endpoint for LOGICAL_DNS cluster has unexpected custom resolver name: {resolver_name}"
        )));
    }

    let hostname = socket_address.address().to_str().unwrap_or_default();
    if hostname.is_empty() {
        return Err(Error::Validation(
            "host for endpoint for LOGICAL_DNS cluster not set".into(),
        ));
    }
    let port = socket_address.port_value();
    if port == 0 {
        return Err(Error::Validation(
            "port for endpoint for LOGICAL_DNS cluster not set".into(),
        ));
    }
    let port = u16::try_from(port).map_err(|_| {
        Error::Validation(format!(
            "port for endpoint for LOGICAL_DNS cluster is out of range: {port}"
        ))
    })?;

    Ok(ClusterDiscovery::LogicalDns {
        hostname: hostname.to_string(),
        port,
    })
}

fn validate_aggregate_discovery(
    custom: crate::generated::envoy::config::cluster::v3::cluster::CustomClusterTypeView<'_>,
) -> xds_client::Result<ClusterDiscovery> {
    if !custom.has_typed_config() {
        return Err(Error::Validation(
            "aggregate cluster missing typed_config".into(),
        ));
    }
    let any = custom.typed_config();
    let type_url = any.type_url().to_str().unwrap_or_default();
    if type_url != AGGREGATE_CLUSTER_CONFIG_TYPE_URL {
        return Err(Error::Validation(format!(
            "unexpected aggregate cluster typed_config type_url: '{type_url}'"
        )));
    }
    let cluster_config = AggregateClusterConfig::parse(any.value()).map_err(|e| {
        Error::Validation(format!("failed to unmarshal aggregate cluster config: {e}"))
    })?;

    let children: Vec<String> = cluster_config
        .clusters()
        .into_iter()
        .map(|c| c.to_str().unwrap_or_default().to_string())
        .collect();
    if children.is_empty() {
        return Err(Error::Validation(
            "aggregate cluster has empty clusters field in response".into(),
        ));
    }

    Ok(ClusterDiscovery::Aggregate { children })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::envoy::config::cluster::v3::cluster::CustomClusterType;
    use crate::generated::envoy::config::cluster::v3::cluster::EdsClusterConfig;
    use crate::generated::envoy::config::core::v3::Address;
    use crate::generated::envoy::config::core::v3::AggregatedConfigSource;
    use crate::generated::envoy::config::core::v3::ConfigSource;
    use crate::generated::envoy::config::core::v3::SocketAddress;
    use crate::generated::envoy::config::endpoint::v3::ClusterLoadAssignment;
    use crate::generated::envoy::config::endpoint::v3::Endpoint;
    use crate::generated::envoy::config::endpoint::v3::LbEndpoint;
    use crate::generated::envoy::config::endpoint::v3::LocalityLbEndpoints;
    use protobuf::Serialize;
    use protobuf_well_known_types::Any;

    fn make_ads_eds_config() -> EdsClusterConfig {
        let mut eds_cfg = EdsClusterConfig::new();
        let mut config_source = ConfigSource::new();
        config_source.set_ads(AggregatedConfigSource::new());
        eds_cfg.set_eds_config(config_source);
        eds_cfg
    }

    fn make_cluster(name: &str) -> Cluster {
        let mut cluster = Cluster::new();
        cluster.set_name(name);
        cluster.set_type(DiscoveryType::Eds);
        cluster.set_eds_cluster_config(make_ads_eds_config());
        cluster
    }

    fn make_logical_dns_cluster(host: &str, port: u32) -> Cluster {
        let mut cluster = Cluster::new();
        cluster.set_name("dns-cluster");
        cluster.set_type(DiscoveryType::LogicalDns);

        let mut socket_address = SocketAddress::new();
        socket_address.set_address(host);
        socket_address.set_port_value(port);
        let mut address = Address::new();
        address.set_socket_address(socket_address);
        let mut endpoint = Endpoint::new();
        endpoint.set_address(address);
        let mut lb_endpoint = LbEndpoint::new();
        lb_endpoint.set_endpoint(endpoint);
        let mut locality_lb_endpoints = LocalityLbEndpoints::new();
        locality_lb_endpoints.lb_endpoints_mut().push(lb_endpoint);
        let mut cla = ClusterLoadAssignment::new();
        cla.endpoints_mut().push(locality_lb_endpoints);
        cluster.set_load_assignment(cla);
        cluster
    }

    #[test]
    fn validate_eds_basic() {
        let cluster = make_cluster("my-cluster");
        let validated = ClusterResource::validate(cluster).expect("should validate");
        assert_eq!(validated.name, "my-cluster");
        match validated.discovery {
            ClusterDiscovery::Eds { eds_service_name } => {
                assert_eq!(eds_service_name, "my-cluster");
            }
            other => panic!("expected Eds, got {other:?}"),
        }
    }

    #[test]
    fn validate_eds_service_name_override() {
        let mut cluster = make_cluster("my-cluster");
        let mut eds_cfg = make_ads_eds_config();
        eds_cfg.set_service_name("eds-svc");
        cluster.set_eds_cluster_config(eds_cfg);
        let validated = ClusterResource::validate(cluster).unwrap();
        match validated.discovery {
            ClusterDiscovery::Eds { eds_service_name } => assert_eq!(eds_service_name, "eds-svc"),
            other => panic!("expected Eds, got {other:?}"),
        }
    }

    #[test]
    fn validate_eds_rejects_missing_eds_config() {
        let mut cluster = Cluster::new();
        cluster.set_name("my-cluster");
        cluster.set_type(DiscoveryType::Eds);
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("EDS config source is not set"));
    }

    #[test]
    fn validate_eds_xdstp_name_requires_service_name() {
        let cluster = make_cluster("xdstp://example.com/envoy.config.cluster.v3.Cluster/foo");
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("new-style cluster name"));
    }

    #[test]
    fn validate_eds_rejects_non_ads_non_self_config_source() {
        let mut cluster = make_cluster("my-cluster");
        let mut eds_cfg = EdsClusterConfig::new();
        let mut config_source = ConfigSource::new();
        config_source.set_path("/some/path");
        eds_cfg.set_eds_config(config_source);
        cluster.set_eds_cluster_config(eds_cfg);
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("not ADS or Self"));
    }

    #[test]
    fn validate_eds_accepts_ads_config_source() {
        let mut cluster = make_cluster("my-cluster");
        let mut eds_cfg = EdsClusterConfig::new();
        let mut config_source = ConfigSource::new();
        config_source.set_ads(AggregatedConfigSource::new());
        eds_cfg.set_eds_config(config_source);
        cluster.set_eds_cluster_config(eds_cfg);
        assert!(ClusterResource::validate(cluster).is_ok());
    }

    #[test]
    fn validate_logical_dns() {
        let cluster = make_logical_dns_cluster("example.com", 443);
        let validated = ClusterResource::validate(cluster).expect("should validate");
        match validated.discovery {
            ClusterDiscovery::LogicalDns { hostname, port } => {
                assert_eq!(hostname, "example.com");
                assert_eq!(port, 443);
            }
            other => panic!("expected LogicalDns, got {other:?}"),
        }
    }

    #[test]
    fn validate_logical_dns_rejects_out_of_range_port() {
        // 65536 must be rejected outright rather than truncated to 0.
        let cluster = make_logical_dns_cluster("example.com", 65_536);
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn validate_logical_dns_missing_load_assignment() {
        let mut cluster = Cluster::new();
        cluster.set_name("dns-cluster");
        cluster.set_type(DiscoveryType::LogicalDns);
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("load_assignment not present"));
    }

    #[test]
    fn validate_aggregate() {
        let mut inner = AggregateClusterConfig::new();
        inner.clusters_mut().push("child-a");
        inner.clusters_mut().push("child-b");

        let mut any = Any::new();
        any.set_type_url(
            "type.googleapis.com/envoy.extensions.clusters.aggregate.v3.ClusterConfig",
        );
        any.set_value(inner.serialize().expect("serialize"));

        let mut custom = CustomClusterType::new();
        custom.set_name("envoy.clusters.aggregate");
        custom.set_typed_config(any);

        let mut cluster = Cluster::new();
        cluster.set_name("aggregate-cluster");
        cluster.set_cluster_type(custom);

        let validated = ClusterResource::validate(cluster).expect("should validate");
        match validated.discovery {
            ClusterDiscovery::Aggregate { children } => {
                assert_eq!(children, vec!["child-a".to_string(), "child-b".to_string()]);
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn validate_aggregate_rejects_unexpected_typed_config_type_url() {
        let mut inner = AggregateClusterConfig::new();
        inner.clusters_mut().push("child-a");

        let mut any = Any::new();
        any.set_type_url("type.googleapis.com/envoy.config.cluster.v3.Cluster");
        any.set_value(inner.serialize().expect("serialize"));

        let mut custom = CustomClusterType::new();
        custom.set_name("envoy.clusters.aggregate");
        custom.set_typed_config(any);

        let mut cluster = Cluster::new();
        cluster.set_name("aggregate-cluster");
        cluster.set_cluster_type(custom);

        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("type_url"));
    }

    #[test]
    fn validate_aggregate_rejects_empty_children() {
        let inner = AggregateClusterConfig::new();
        let mut any = Any::new();
        any.set_type_url(
            "type.googleapis.com/envoy.extensions.clusters.aggregate.v3.ClusterConfig",
        );
        any.set_value(inner.serialize().expect("serialize"));

        let mut custom = CustomClusterType::new();
        custom.set_name("envoy.clusters.aggregate");
        custom.set_typed_config(any);

        let mut cluster = Cluster::new();
        cluster.set_name("aggregate-cluster");
        cluster.set_cluster_type(custom);

        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("empty clusters field"));
    }

    #[test]
    fn validate_empty_name() {
        let cluster = make_cluster("");
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("cluster name is empty"));
    }

    #[test]
    fn validate_unsupported_discovery_type_rejected() {
        let mut cluster = Cluster::new();
        cluster.set_name("static-cluster");
        cluster.set_type(DiscoveryType::Static);
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported cluster discovery type")
        );
    }

    #[test]
    fn deserialize_roundtrip() {
        let cluster = make_cluster("test");
        let bytes = cluster.serialize().expect("serialize");
        let deserialized = ClusterResource::deserialize(bytes::Bytes::from(bytes)).unwrap();
        assert_eq!(ClusterResource::name(&deserialized), "test");
    }
}
