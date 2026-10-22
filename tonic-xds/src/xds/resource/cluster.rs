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

use bytes::Bytes;
use envoy_types::pb::envoy::config::cluster::v3::{Cluster, cluster};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use super::security::{ClusterSecurityConfig, parse_transport_socket};

// Re-exported so other modules keep using the `cluster::LbPolicy` path.
pub(crate) use super::lb_policy::{LbPolicy, RingHashSettings};

/// Gate for accepting RING_HASH clusters. Flip to `true` once ring-hash is wired
/// into the client; until then such clusters are NACKed.
const RING_HASH_ENABLED: bool = false;

/// Validated Cluster resource.
#[derive(Debug, Clone)]
pub(crate) struct ClusterResource {
    pub name: String,
    /// The EDS service name for endpoint discovery.
    /// If not set, the cluster name is used.
    pub eds_service_name: Option<String>,
    /// The load balancing policy for this cluster.
    pub lb_policy: LbPolicy,
    /// TLS security config parsed from `transport_socket`. `None` means the
    /// cluster uses plaintext connections.
    pub security: Option<ClusterSecurityConfig>,
}

impl Resource for ClusterResource {
    type Message = Cluster;

    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.cluster.v3.Cluster");

    const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

    fn deserialize(bytes: Bytes) -> xds_client::Result<Self::Message> {
        Cluster::decode(bytes).map_err(Into::into)
    }

    fn name(message: &Self::Message) -> &str {
        &message.name
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let name = message.name;
        if name.is_empty() {
            return Err(Error::Validation("cluster name is empty".into()));
        }

        let eds_service_name = message
            .eds_cluster_config
            .map(|eds| eds.service_name)
            .filter(|s| !s.is_empty());

        let lb_policy = match cluster::LbPolicy::try_from(message.lb_policy) {
            Ok(cluster::LbPolicy::RoundRobin) => LbPolicy::RoundRobin,
            Ok(cluster::LbPolicy::LeastRequest) => LbPolicy::LeastRequest,
            Ok(cluster::LbPolicy::RingHash) => {
                let settings = RingHashSettings::validate(message.lb_config)?;
                if !RING_HASH_ENABLED {
                    return Err(Error::Validation("RING_HASH is not yet supported".into()));
                }
                LbPolicy::RingHash(settings)
            }
            _ => {
                return Err(Error::Validation(format!(
                    "unsupported load balancing policy: {}",
                    message.lb_policy
                )));
            }
        };

        let security = parse_transport_socket(message.transport_socket)?;

        Ok(ClusterResource {
            name,
            eds_service_name,
            lb_policy,
            security,
        })
    }
}

impl ClusterResource {
    /// Returns the EDS service name for cascading EDS subscriptions.
    /// Falls back to the cluster name if no EDS service name is set.
    pub(crate) fn eds_service_name(&self) -> &str {
        self.eds_service_name.as_deref().unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use envoy_types::pb::envoy::config::cluster::v3::cluster::EdsClusterConfig;

    use super::*;

    fn make_cluster(name: &str) -> Cluster {
        Cluster {
            name: name.to_string(),
            lb_policy: cluster::LbPolicy::RoundRobin as i32,
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_basic() {
        let cluster = make_cluster("my-cluster");
        let validated = ClusterResource::validate(cluster).expect("should validate");
        assert_eq!(validated.name, "my-cluster");
        assert_eq!(validated.lb_policy, LbPolicy::RoundRobin);
        assert!(validated.eds_service_name.is_none());
    }

    #[test]
    fn test_eds_service_name_defaults_to_cluster_name() {
        let cluster = make_cluster("my-cluster");
        let validated = ClusterResource::validate(cluster).unwrap();
        assert_eq!(validated.eds_service_name(), "my-cluster");
    }

    #[test]
    fn test_eds_service_name() {
        let cluster = Cluster {
            name: "my-cluster".to_string(),
            eds_cluster_config: Some(EdsClusterConfig {
                service_name: "eds-svc".to_string(),
                ..Default::default()
            }),
            lb_policy: cluster::LbPolicy::RoundRobin as i32,
            ..Default::default()
        };
        let validated = ClusterResource::validate(cluster).unwrap();
        assert_eq!(validated.eds_service_name.as_deref(), Some("eds-svc"));
        assert_eq!(validated.eds_service_name(), "eds-svc");
    }

    #[test]
    fn test_least_request_lb_policy() {
        let cluster = Cluster {
            name: "lr-cluster".to_string(),
            lb_policy: cluster::LbPolicy::LeastRequest as i32,
            ..Default::default()
        };
        let validated = ClusterResource::validate(cluster).unwrap();
        assert_eq!(validated.lb_policy, LbPolicy::LeastRequest);
    }

    #[test]
    fn test_unsupported_lb_policy_is_rejected() {
        // A policy we don't support (e.g. MAGLEV) is still NACKed.
        let cluster = Cluster {
            name: "mg-cluster".to_string(),
            lb_policy: cluster::LbPolicy::Maglev as i32,
            ..Default::default()
        };
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported load balancing policy")
        );
    }

    #[test]
    fn test_ring_hash_lb_policy_nacked_while_disabled() {
        // Valid ring-hash config is still rejected until RING_HASH_ENABLED is
        // flipped. Detailed config validation is covered in `lb_policy`.
        let cluster = Cluster {
            name: "rh-cluster".to_string(),
            lb_policy: cluster::LbPolicy::RingHash as i32,
            ..Default::default()
        };
        let result = ClusterResource::validate(cluster);
        if RING_HASH_ENABLED {
            let validated = result.unwrap();
            assert!(matches!(validated.lb_policy, LbPolicy::RingHash(_)));
        } else {
            let err = result.unwrap_err();
            assert!(err.to_string().contains("RING_HASH is not yet supported"));
        }
    }

    #[test]
    fn test_validate_empty_name() {
        let cluster = make_cluster("");
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("cluster name is empty"));
    }

    #[test]
    fn test_deserialize_roundtrip() {
        let cluster = make_cluster("test");
        let bytes = cluster.encode_to_vec();
        let deserialized = ClusterResource::deserialize(Bytes::from(bytes)).unwrap();
        assert_eq!(ClusterResource::name(&deserialized), "test");
    }
}
