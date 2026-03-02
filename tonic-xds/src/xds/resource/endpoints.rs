//! Validated ClusterLoadAssignment resource (EDS).

use bytes::Bytes;
use envoy_types::pb::envoy::config::core::v3::{
    address, socket_address, HealthStatus as EnvoyHealthStatus,
};
use envoy_types::pb::envoy::config::endpoint::v3::{
    lb_endpoint, ClusterLoadAssignment, LbEndpoint,
};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use crate::client::endpoint::EndpointAddress;

/// Validated ClusterLoadAssignment (EDS resource).
#[derive(Debug, Clone)]
pub(crate) struct EndpointsResource {
    pub cluster_name: String,
    pub localities: Vec<LocalityEndpoints>,
}

/// Endpoints within a locality.
#[derive(Debug, Clone)]
pub(crate) struct LocalityEndpoints {
    pub endpoints: Vec<ResolvedEndpoint>,
    pub load_balancing_weight: u32,
    pub priority: u32,
}

/// A single validated endpoint.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedEndpoint {
    pub address: EndpointAddress,
    pub health_status: HealthStatus,
    pub load_balancing_weight: u32,
}

/// Health status of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealthStatus {
    Unknown,
    Healthy,
    Unhealthy,
    Draining,
}

impl From<i32> for HealthStatus {
    fn from(value: i32) -> Self {
        match EnvoyHealthStatus::try_from(value) {
            Ok(EnvoyHealthStatus::Healthy) => Self::Healthy,
            // Envoy's health_check.proto defines TIMEOUT as "interpreted by Envoy as
            // UNHEALTHY". Per gRFC A27, only HEALTHY and UNKNOWN are considered usable.
            Ok(EnvoyHealthStatus::Unhealthy) | Ok(EnvoyHealthStatus::Timeout) => Self::Unhealthy,
            Ok(EnvoyHealthStatus::Draining) => Self::Draining,
            _ => Self::Unknown,
        }
    }
}

impl Resource for EndpointsResource {
    type Message = ClusterLoadAssignment;

    const TYPE_URL: TypeUrl =
        TypeUrl::new("type.googleapis.com/envoy.config.endpoint.v3.ClusterLoadAssignment");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

    fn deserialize(bytes: Bytes) -> xds_client::Result<Self::Message> {
        ClusterLoadAssignment::decode(bytes).map_err(Into::into)
    }

    fn name(message: &Self::Message) -> &str {
        &message.cluster_name
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let cluster_name = message.cluster_name;
        if cluster_name.is_empty() {
            return Err(Error::Validation(
                "ClusterLoadAssignment missing cluster_name".into(),
            ));
        }

        let mut localities = Vec::with_capacity(message.endpoints.len());
        for locality_endpoints in message.endpoints {
            let mut endpoints = Vec::with_capacity(locality_endpoints.lb_endpoints.len());
            for lb_ep in locality_endpoints.lb_endpoints {
                if let Some(ep) = validate_lb_endpoint(lb_ep)? {
                    endpoints.push(ep);
                }
            }

            let weight = locality_endpoints
                .load_balancing_weight
                .map(|w| w.value)
                .unwrap_or(0);

            localities.push(LocalityEndpoints {
                endpoints,
                load_balancing_weight: weight,
                priority: locality_endpoints.priority,
            });
        }

        Ok(EndpointsResource {
            cluster_name,
            localities,
        })
    }
}

fn validate_lb_endpoint(lb_ep: LbEndpoint) -> xds_client::Result<Option<ResolvedEndpoint>> {
    let health_status = HealthStatus::from(lb_ep.health_status);

    let host_identifier = match lb_ep.host_identifier {
        Some(lb_endpoint::HostIdentifier::Endpoint(ep)) => ep,
        // Named endpoints not supported.
        _ => return Ok(None),
    };

    let addr = host_identifier
        .address
        .ok_or_else(|| Error::Validation("endpoint missing address".into()))?;

    let addr = match addr.address {
        Some(address::Address::SocketAddress(sa)) => {
            let port = match sa.port_specifier {
                Some(socket_address::PortSpecifier::PortValue(p)) => p as u16,
                _ => {
                    return Err(Error::Validation(
                        "endpoint address missing numeric port".into(),
                    ))
                }
            };
            EndpointAddress::new(sa.address, port)
        }
        _ => {
            return Err(Error::Validation(
                "only socket addresses are supported for gRPC endpoints".into(),
            ))
        }
    };

    let weight = lb_ep.load_balancing_weight.map(|w| w.value).unwrap_or(1);

    Ok(Some(ResolvedEndpoint {
        address: addr,
        health_status,
        load_balancing_weight: weight,
    }))
}

impl EndpointsResource {
    /// Returns all healthy endpoints (Unknown and Healthy status).
    pub(crate) fn healthy_endpoints(&self) -> impl Iterator<Item = &ResolvedEndpoint> {
        self.localities
            .iter()
            .flat_map(|l| &l.endpoints)
            .filter(|e| {
                matches!(
                    e.health_status,
                    HealthStatus::Unknown | HealthStatus::Healthy
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::config::core::v3::{Address, SocketAddress};
    use envoy_types::pb::envoy::config::endpoint::v3::{Endpoint, LocalityLbEndpoints};
    use envoy_types::pb::google::protobuf::UInt32Value;

    fn make_lb_endpoint(ip: &str, port: u32, health: i32) -> LbEndpoint {
        LbEndpoint {
            host_identifier: Some(lb_endpoint::HostIdentifier::Endpoint(Endpoint {
                address: Some(Address {
                    address: Some(address::Address::SocketAddress(SocketAddress {
                        address: ip.to_string(),
                        port_specifier: Some(socket_address::PortSpecifier::PortValue(port)),
                        ..Default::default()
                    })),
                }),
                ..Default::default()
            })),
            health_status: health,
            load_balancing_weight: Some(UInt32Value { value: 1 }),
            ..Default::default()
        }
    }

    fn make_cla(cluster_name: &str) -> ClusterLoadAssignment {
        ClusterLoadAssignment {
            cluster_name: cluster_name.to_string(),
            endpoints: vec![LocalityLbEndpoints {
                lb_endpoints: vec![
                    make_lb_endpoint("10.0.0.1", 8080, EnvoyHealthStatus::Healthy as i32),
                    make_lb_endpoint("10.0.0.2", 8080, EnvoyHealthStatus::Unknown as i32),
                    make_lb_endpoint("10.0.0.3", 8080, EnvoyHealthStatus::Unhealthy as i32),
                ],
                load_balancing_weight: Some(UInt32Value { value: 100 }),
                priority: 0,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_basic() {
        let cla = make_cla("my-cluster");
        let validated = EndpointsResource::validate(cla).expect("should validate");
        assert_eq!(validated.cluster_name, "my-cluster");
        assert_eq!(validated.localities.len(), 1);
        assert_eq!(validated.localities[0].endpoints.len(), 3);
    }

    #[test]
    fn test_healthy_endpoints() {
        let cla = make_cla("my-cluster");
        let validated = EndpointsResource::validate(cla).unwrap();
        let healthy: Vec<_> = validated.healthy_endpoints().collect();
        // Healthy + Unknown = 2 (Unhealthy excluded)
        assert_eq!(healthy.len(), 2);
    }

    #[test]
    fn test_validate_empty_cluster_name() {
        let cla = ClusterLoadAssignment {
            cluster_name: String::new(),
            ..Default::default()
        };
        let err = EndpointsResource::validate(cla).unwrap_err();
        assert!(err.to_string().contains("cluster_name"));
    }

    #[test]
    fn test_not_all_resources_required() {
        assert!(!EndpointsResource::ALL_RESOURCES_REQUIRED_IN_SOTW);
    }

    #[test]
    fn test_deserialize_roundtrip() {
        let cla = make_cla("test");
        let bytes = cla.encode_to_vec();
        let deserialized = EndpointsResource::deserialize(Bytes::from(bytes)).unwrap();
        assert_eq!(EndpointsResource::name(&deserialized), "test");
    }

    #[test]
    fn test_endpoint_with_weight() {
        let cla = make_cla("c1");
        let validated = EndpointsResource::validate(cla).unwrap();
        for ep in &validated.localities[0].endpoints {
            assert_eq!(ep.load_balancing_weight, 1);
        }
    }
}
