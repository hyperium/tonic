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

//! Validated ClusterLoadAssignment resource (EDS).

use bytes::Bytes;
use envoy_types::pb::envoy::config::core::v3::{
    Address, HealthStatus as EnvoyHealthStatus, address, socket_address,
};
use envoy_types::pb::envoy::config::endpoint::v3::{
    ClusterLoadAssignment, LbEndpoint, lb_endpoint,
};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use crate::client::endpoint::EndpointAddress;

/// Validated ClusterLoadAssignment (EDS resource).
#[derive(Debug)]
pub(crate) struct EndpointsResource {
    pub cluster_name: String,
    pub localities: Vec<LocalityEndpoints>,
}

/// Endpoints within a locality.
#[derive(Debug)]
pub(crate) struct LocalityEndpoints {
    pub locality: Option<Locality>,
    pub endpoints: Vec<ResolvedEndpoint>,
    pub load_balancing_weight: u32,
    pub priority: u32,
}

/// Locality information for a set of endpoints.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct Locality {
    pub region: String,
    pub zone: String,
    pub sub_zone: String,
}

/// A single validated endpoint.
#[derive(Debug)]
pub(crate) struct ResolvedEndpoint {
    pub address: EndpointAddress,
    /// Further addresses of the same backend from `Endpoint.additional_addresses`
    /// (gRFC A61), in control-plane order.
    pub additional_addresses: Vec<EndpointAddress>,
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

            let locality = locality_endpoints.locality.map(|l| Locality {
                region: l.region,
                zone: l.zone,
                sub_zone: l.sub_zone,
            });

            localities.push(LocalityEndpoints {
                locality,
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
        // Skip unsupported or missing host_identifier variants (e.g. named
        // endpoints). These are not relevant to gRPC and should not cause the
        // entire EDS resource to be NACKed — the control plane may be serving
        // both envoy proxies and gRPC clients.
        _ => return Ok(None),
    };

    let address = parse_address(host_identifier.address)
        .map_err(|reason| Error::Validation(format!("endpoint address: {reason}")))?;

    // gRFC A61: each entry is validated exactly like `Endpoint.address`.
    let additional_addresses = host_identifier
        .additional_addresses
        .into_iter()
        .enumerate()
        .map(|(i, additional)| {
            parse_address(additional.address).map_err(|reason| {
                Error::Validation(format!("endpoint additional_addresses[{i}]: {reason}"))
            })
        })
        .collect::<xds_client::Result<Vec<_>>>()?;

    let weight = lb_ep.load_balancing_weight.map(|w| w.value).unwrap_or(1);

    Ok(Some(ResolvedEndpoint {
        address,
        additional_addresses,
        health_status,
        load_balancing_weight: weight,
    }))
}

/// Validates an endpoint `Address`, returning the rejection reason on failure.
fn parse_address(address: Option<Address>) -> Result<EndpointAddress, &'static str> {
    let address = address.ok_or("address is unset")?;
    match address.address {
        Some(address::Address::SocketAddress(sa)) => match sa.port_specifier {
            Some(socket_address::PortSpecifier::PortValue(port)) => {
                Ok(EndpointAddress::new(sa.address, port as u16))
            }
            _ => Err("missing numeric port"),
        },
        _ => Err("only socket addresses are supported for gRPC endpoints"),
    }
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
    use envoy_types::pb::envoy::config::core::v3::{Pipe, SocketAddress};
    use envoy_types::pb::envoy::config::endpoint::v3::endpoint::AdditionalAddress;
    use envoy_types::pb::envoy::config::endpoint::v3::{Endpoint, LocalityLbEndpoints};
    use envoy_types::pb::google::protobuf::UInt32Value;

    use super::*;

    fn make_socket_address(ip: &str, port: u32) -> Address {
        Address {
            address: Some(address::Address::SocketAddress(SocketAddress {
                address: ip.to_string(),
                port_specifier: Some(socket_address::PortSpecifier::PortValue(port)),
                ..Default::default()
            })),
        }
    }

    fn make_lb_endpoint(ip: &str, port: u32, health: i32) -> LbEndpoint {
        LbEndpoint {
            host_identifier: Some(lb_endpoint::HostIdentifier::Endpoint(Endpoint {
                address: Some(make_socket_address(ip, port)),
                ..Default::default()
            })),
            health_status: health,
            load_balancing_weight: Some(UInt32Value { value: 1 }),
            ..Default::default()
        }
    }

    /// Builds a single-endpoint resource whose endpoint carries the given
    /// `additional_addresses` entries.
    fn make_cla_with_additional(additional: Vec<Option<Address>>) -> ClusterLoadAssignment {
        let mut lb_ep = make_lb_endpoint("10.0.0.1", 8080, EnvoyHealthStatus::Healthy as i32);
        let Some(lb_endpoint::HostIdentifier::Endpoint(ep)) = &mut lb_ep.host_identifier else {
            unreachable!("make_lb_endpoint always sets an Endpoint");
        };
        ep.additional_addresses = additional
            .into_iter()
            .map(|address| AdditionalAddress { address })
            .collect();
        ClusterLoadAssignment {
            cluster_name: "c1".to_string(),
            endpoints: vec![LocalityLbEndpoints {
                lb_endpoints: vec![lb_ep],
                ..Default::default()
            }],
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

    #[test]
    fn test_no_additional_addresses() {
        let validated = EndpointsResource::validate(make_cla("c1")).unwrap();
        for ep in &validated.localities[0].endpoints {
            assert!(ep.additional_addresses.is_empty());
        }
    }

    #[test]
    fn test_additional_addresses_parsed_in_order() {
        let cla = make_cla_with_additional(vec![
            Some(make_socket_address("2001:db8::1", 8080)),
            Some(make_socket_address("10.0.1.1", 9090)),
        ]);
        let validated = EndpointsResource::validate(cla).unwrap();
        let ep = &validated.localities[0].endpoints[0];
        assert_eq!(ep.address, EndpointAddress::new("10.0.0.1", 8080));
        assert_eq!(
            ep.additional_addresses,
            vec![
                EndpointAddress::new("2001:db8::1", 8080),
                EndpointAddress::new("10.0.1.1", 9090),
            ]
        );
    }

    #[test]
    fn test_invalid_additional_address_rejected() {
        let named_port = Address {
            address: Some(address::Address::SocketAddress(SocketAddress {
                address: "10.0.1.1".to_string(),
                port_specifier: Some(socket_address::PortSpecifier::NamedPort("grpc".into())),
                ..Default::default()
            })),
        };
        let pipe = Address {
            address: Some(address::Address::Pipe(Pipe {
                path: "/tmp/grpc.sock".to_string(),
                ..Default::default()
            })),
        };
        let cases = [
            (None, "address is unset"),
            (Some(Address::default()), "only socket addresses"),
            (Some(pipe), "only socket addresses"),
            (Some(named_port), "missing numeric port"),
        ];
        for (invalid, reason) in cases {
            // Put the invalid entry second so the error must name its index.
            let cla = make_cla_with_additional(vec![
                Some(make_socket_address("10.0.1.2", 8080)),
                invalid,
            ]);
            let err = EndpointsResource::validate(cla).unwrap_err().to_string();
            assert!(err.contains("additional_addresses[1]"), "{err}");
            assert!(err.contains(reason), "{err}");
        }
    }
}
