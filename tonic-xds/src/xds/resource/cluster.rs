//! Validated Cluster resource (CDS).

use bytes::Bytes;
use envoy_types::pb::envoy::config::cluster::v3::{Cluster, cluster};
use prost::Message;
use xds_client::resource::TypeUrl;
use xds_client::{Error, Resource};

use super::security::{ClusterSecurityConfig, parse_transport_socket};

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

/// Load balancing policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LbPolicy {
    RoundRobin,
    LeastRequest,
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
    use super::*;
    use envoy_types::pb::envoy::config::cluster::v3::cluster::EdsClusterConfig;

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
        let cluster = Cluster {
            name: "rh-cluster".to_string(),
            lb_policy: cluster::LbPolicy::RingHash as i32,
            ..Default::default()
        };
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported load balancing policy")
        );
    }

    #[test]
    fn test_validate_empty_name() {
        let cluster = make_cluster("");
        let err = ClusterResource::validate(cluster).unwrap_err();
        assert!(err.to_string().contains("cluster name is empty"));
    }

    #[test]
    fn test_all_resources_required() {
        assert!(ClusterResource::ALL_RESOURCES_REQUIRED_IN_SOTW);
    }

    #[test]
    fn test_deserialize_roundtrip() {
        let cluster = make_cluster("test");
        let bytes = cluster.encode_to_vec();
        let deserialized = ClusterResource::deserialize(Bytes::from(bytes)).unwrap();
        assert_eq!(ClusterResource::name(&deserialized), "test");
    }
}
