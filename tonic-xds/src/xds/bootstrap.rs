//! xDS bootstrap configuration.
//!
//! Parses the bootstrap JSON from `GRPC_XDS_BOOTSTRAP` (file path) or
//! `GRPC_XDS_BOOTSTRAP_CONFIG` (inline JSON) environment variables,
//! per gRFC A27.

use std::collections::HashMap;

use serde::Deserialize;
use xds_client::message::{Locality, Node};

/// Environment variable pointing to a bootstrap JSON file path.
const ENV_BOOTSTRAP_FILE: &str = "GRPC_XDS_BOOTSTRAP";
/// Environment variable containing inline bootstrap JSON.
const ENV_BOOTSTRAP_CONFIG: &str = "GRPC_XDS_BOOTSTRAP_CONFIG";

/// Parsed xDS bootstrap configuration per [gRFC A27].
///
/// The bootstrap tells the xDS client where the management server lives
/// and what identity (node) to present. It is typically loaded from a
/// JSON file or environment variable.
///
/// # Loading
///
/// ```rust,no_run
/// use tonic_xds::BootstrapConfig;
///
/// // From environment variable (GRPC_XDS_BOOTSTRAP or GRPC_XDS_BOOTSTRAP_CONFIG):
/// let config = BootstrapConfig::from_env().unwrap();
///
/// // From a JSON string:
/// let json = r#"{"xds_servers":[{"server_uri":"xds.example.com:443"}]}"#;
/// let config = BootstrapConfig::from_json(json).unwrap();
/// ```
///
/// [gRFC A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
// TODO: Design a public builder API for constructing BootstrapConfig
// programmatically (not just from JSON). The current `new()` is pub(crate);
// a public API should use the builder pattern to accommodate future fields
// without breaking changes.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BootstrapConfig {
    /// xDS management servers to connect to.
    pub(crate) xds_servers: Vec<XdsServerConfig>,
    /// Node identity sent to the xDS server.
    #[serde(default)]
    pub(crate) node: NodeConfig,
    /// Certificate provider plugin instances, keyed by instance name.
    ///
    /// Referenced by [`CertificateProviderPluginInstance`] in CDS/LDS
    /// `UpstreamTlsContext` / `DownstreamTlsContext` resources.
    /// See gRFC A29 for details.
    ///
    /// [`CertificateProviderPluginInstance`]: https://github.com/envoyproxy/envoy/blob/main/api/envoy/extensions/transport_sockets/tls/v3/common.proto
    #[serde(default)]
    #[allow(dead_code)] // Consumed when CertProviderRegistry is wired in (PR2/A29).
    pub(crate) certificate_providers: HashMap<String, CertProviderPluginConfig>,
}

/// Configuration for a single xDS management server.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct XdsServerConfig {
    /// URI of the xDS server (e.g., `"xds.example.com:443"`).
    pub server_uri: String,
    /// Ordered list of channel credentials. The client uses the first supported type.
    #[serde(default)]
    pub channel_creds: Vec<ChannelCredentialConfig>,
    /// Server features (e.g., `["xds_v3"]`).
    #[serde(default)]
    #[allow(dead_code)]
    // Parsed for completeness; used when server feature negotiation is added.
    pub server_features: Vec<String>,
}

/// A channel credential entry from the bootstrap config.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChannelCredentialConfig {
    /// Credential type (e.g., `"insecure"`, `"tls"`, `"google_default"`).
    #[serde(rename = "type")]
    pub cred_type: ChannelCredentialType,
}

/// Channel credential type from the bootstrap config.
///
/// Known types are deserialized into specific variants; unrecognized types
/// are captured as `Unsupported(String)` so they can be skipped gracefully.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChannelCredentialType {
    Insecure,
    Tls,
    #[serde(untagged)]
    Unsupported(String),
}

/// A certificate provider plugin entry from the bootstrap config.
///
/// Holds the `plugin_name` and an opaque `config` blob. The cert provider
/// module is responsible for dispatching on `plugin_name` and deserializing
/// `config` into the appropriate plugin-specific type.
///
/// Referenced by `instance_name` in CDS/LDS `CertificateProviderPluginInstance`
/// fields. See [gRFC A29].
///
/// [gRFC A29]: https://github.com/grpc/proposal/blob/master/A29-xds-tls-security.md
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CertProviderPluginConfig {
    pub plugin_name: String,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Node identity configuration from bootstrap JSON.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct NodeConfig {
    /// Opaque node identifier.
    #[serde(default)]
    pub id: String,
    /// Cluster the node belongs to.
    pub cluster: Option<String>,
    /// Locality where the node is running.
    pub locality: Option<LocalityConfig>,
    /// Free-form metadata sent to the xDS server (`google.protobuf.Struct`).
    ///
    /// Only string values are supported here; nested structs and other Value
    /// kinds are not exposed. Some control planes vary the served config based
    /// on metadata — e.g. Istio's istiod gates proxyless gRPC config behind
    /// `GENERATOR = "grpc"`.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Locality configuration from bootstrap JSON.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LocalityConfig {
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub zone: String,
    #[serde(default)]
    pub sub_zone: String,
}

/// Errors that can occur when loading bootstrap configuration.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// Neither `GRPC_XDS_BOOTSTRAP` nor `GRPC_XDS_BOOTSTRAP_CONFIG` is set.
    #[error("neither {ENV_BOOTSTRAP_FILE} nor {ENV_BOOTSTRAP_CONFIG} environment variable is set")]
    NotConfigured,
    /// Failed to read the bootstrap JSON file.
    #[error("failed to read bootstrap file '{path}': {source}")]
    ReadFile {
        /// Path that could not be read.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The JSON could not be parsed.
    #[error("failed to parse bootstrap JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// The parsed config failed validation (e.g., empty `xds_servers`).
    #[error("bootstrap config validation failed: {0}")]
    Validation(String),
}

impl BootstrapConfig {
    /// Create a bootstrap configuration directly from struct fields.
    #[allow(dead_code)] // Used by callers constructing config programmatically.
    pub(crate) fn new(
        xds_servers: Vec<XdsServerConfig>,
        node: NodeConfig,
    ) -> Result<Self, BootstrapError> {
        let config = Self {
            xds_servers,
            node,
            certificate_providers: HashMap::new(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Load bootstrap configuration from environment variables.
    ///
    /// Checks `GRPC_XDS_BOOTSTRAP` (file path) first, then falls back to
    /// `GRPC_XDS_BOOTSTRAP_CONFIG` (inline JSON).
    pub fn from_env() -> Result<Self, BootstrapError> {
        if let Ok(path) = std::env::var(ENV_BOOTSTRAP_FILE) {
            let json = std::fs::read_to_string(&path)
                .map_err(|e| BootstrapError::ReadFile { path, source: e })?;
            return Self::from_json(&json);
        }

        if let Ok(json) = std::env::var(ENV_BOOTSTRAP_CONFIG) {
            return Self::from_json(&json);
        }

        Err(BootstrapError::NotConfigured)
    }

    /// Parse bootstrap configuration from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, BootstrapError> {
        let config: BootstrapConfig = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), BootstrapError> {
        if self.xds_servers.is_empty() {
            return Err(BootstrapError::Validation(
                "xds_servers must not be empty".into(),
            ));
        }
        for (i, server) in self.xds_servers.iter().enumerate() {
            if server.server_uri.is_empty() {
                return Err(BootstrapError::Validation(format!(
                    "xds_servers[{i}].server_uri must not be empty"
                )));
            }
        }
        Ok(())
    }

    /// Returns the URI of the first xDS server.
    pub(crate) fn server_uri(&self) -> &str {
        self.xds_servers
            .first()
            .map(|s| s.server_uri.as_str())
            .expect("xds_servers validated non-empty")
    }

    /// Select the first supported channel credential type from the first server's config.
    ///
    /// Per gRFC A27, the client stops at the first credential type it supports.
    /// Returns `None` if no supported credential type is found.
    pub(crate) fn selected_credential(&self) -> Option<&ChannelCredentialType> {
        self.xds_servers
            .first()?
            .channel_creds
            .iter()
            .map(|c| &c.cred_type)
            .find(|t| {
                matches!(
                    t,
                    ChannelCredentialType::Insecure | ChannelCredentialType::Tls
                )
            })
    }

    /// Returns `true` if the first server's selected credential is TLS.
    pub(crate) fn use_tls(&self) -> bool {
        self.selected_credential() == Some(&ChannelCredentialType::Tls)
    }
}

impl From<NodeConfig> for Node {
    fn from(config: NodeConfig) -> Self {
        let mut node = Node::new("tonic-xds", env!("CARGO_PKG_VERSION"));

        if !config.id.is_empty() {
            node = node.with_id(config.id);
        }
        if let Some(cluster) = config.cluster {
            node = node.with_cluster(cluster);
        }
        if let Some(locality) = config.locality {
            node = node.with_locality(Locality {
                region: locality.region,
                zone: locality.zone,
                sub_zone: locality.sub_zone,
            });
        }
        if !config.metadata.is_empty() {
            node = node.with_metadata(config.metadata);
        }

        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_json() -> &'static str {
        r#"{
            "xds_servers": [{"server_uri": "xds.example.com:443"}],
            "node": {"id": "test-node"}
        }"#
    }

    fn full_json() -> &'static str {
        r#"{
            "xds_servers": [{
                "server_uri": "xds.example.com:443",
                "channel_creds": [
                    {"type": "google_default"},
                    {"type": "tls"},
                    {"type": "insecure"}
                ],
                "server_features": ["xds_v3"]
            }],
            "node": {
                "id": "projects/123/nodes/456",
                "cluster": "test-cluster",
                "locality": {
                    "region": "us-east1",
                    "zone": "us-east1-b",
                    "sub_zone": "rack1"
                }
            }
        }"#
    }

    #[test]
    fn parse_minimal() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        assert_eq!(config.xds_servers.len(), 1);
        assert_eq!(config.server_uri(), "xds.example.com:443");
        assert_eq!(config.node.id, "test-node");
        assert!(config.node.cluster.is_none());
        assert!(config.node.locality.is_none());
    }

    #[test]
    fn parse_full() {
        let config = BootstrapConfig::from_json(full_json()).unwrap();
        assert_eq!(config.xds_servers[0].server_uri, "xds.example.com:443");
        assert_eq!(config.xds_servers[0].channel_creds.len(), 3);
        assert!(matches!(
            config.xds_servers[0].channel_creds[0].cred_type,
            ChannelCredentialType::Unsupported(_)
        ));
        assert_eq!(config.xds_servers[0].server_features, vec!["xds_v3"]);
        assert_eq!(config.node.id, "projects/123/nodes/456");
        assert_eq!(config.node.cluster.as_deref(), Some("test-cluster"));

        let locality = config.node.locality.as_ref().unwrap();
        assert_eq!(locality.region, "us-east1");
        assert_eq!(locality.zone, "us-east1-b");
        assert_eq!(locality.sub_zone, "rack1");
    }

    #[test]
    fn node_from_full_config() {
        let config = BootstrapConfig::from_json(full_json()).unwrap();
        let node = Node::from(config.node);
        assert_eq!(node.id.as_deref(), Some("projects/123/nodes/456"));
        assert_eq!(node.cluster.as_deref(), Some("test-cluster"));
        assert_eq!(node.user_agent_name, "tonic-xds");

        let locality = node.locality.unwrap();
        assert_eq!(locality.region, "us-east1");
        assert_eq!(locality.zone, "us-east1-b");
        assert_eq!(locality.sub_zone, "rack1");
    }

    #[test]
    fn node_from_minimal_config() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        let node = Node::from(config.node);
        assert_eq!(node.id.as_deref(), Some("test-node"));
        assert!(node.cluster.is_none());
        assert!(node.locality.is_none());
    }

    #[test]
    fn selected_credential_first_supported_wins() {
        let config = BootstrapConfig::from_json(full_json()).unwrap();
        // google_default skipped, tls is first supported
        assert_eq!(
            config.selected_credential(),
            Some(&ChannelCredentialType::Tls)
        );
    }

    #[test]
    fn selected_credential_insecure() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
            "node": {"id": "n1"}
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(
            config.selected_credential(),
            Some(&ChannelCredentialType::Insecure)
        );
    }

    #[test]
    fn selected_credential_none_supported() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "google_default"}]
            }],
            "node": {"id": "n1"}
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(config.selected_credential(), None);
    }

    #[test]
    fn selected_credential_empty_creds() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        assert_eq!(config.selected_credential(), None);
    }

    #[test]
    fn empty_xds_servers_fails() {
        let json = r#"{"xds_servers": [], "node": {"id": "n1"}}"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("xds_servers must not be empty"));
    }

    #[test]
    fn empty_server_uri_fails() {
        let json = r#"{"xds_servers": [{"server_uri": ""}], "node": {"id": "n1"}}"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("server_uri must not be empty"));
    }

    #[test]
    fn invalid_json_fails() {
        let err = BootstrapConfig::from_json("not json").unwrap_err();
        assert!(matches!(err, BootstrapError::InvalidJson(_)));
    }

    #[test]
    fn missing_required_field_fails() {
        let json = r#"{"node": {"id": "n1"}}"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("xds_servers"));
    }

    #[test]
    fn node_without_id() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}]
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let node = Node::from(config.node);
        assert!(node.id.is_none());
    }

    #[test]
    fn parse_node_metadata() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}],
            "node": {
                "id": "n1",
                "metadata": {
                    "GENERATOR": "grpc",
                    "PILOT_VERSION": "1.20"
                }
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(config.node.metadata.get("GENERATOR").unwrap(), "grpc");
        assert_eq!(config.node.metadata.get("PILOT_VERSION").unwrap(), "1.20");
    }

    #[test]
    fn node_from_config_propagates_metadata() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}],
            "node": {
                "id": "n1",
                "metadata": {"GENERATOR": "grpc"}
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let node = Node::from(config.node);
        assert_eq!(node.metadata.get("GENERATOR").unwrap(), "grpc");
    }

    #[test]
    fn missing_metadata_defaults_to_empty() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        assert!(config.node.metadata.is_empty());
        let node = Node::from(config.node);
        assert!(node.metadata.is_empty());
    }

    #[test]
    fn parse_certificate_providers() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}],
            "certificate_providers": {
                "google_cloud_private_spiffe": {
                    "plugin_name": "file_watcher",
                    "config": {
                        "certificate_file": "/var/run/certs/certificates.pem",
                        "private_key_file": "/var/run/certs/private_key.pem",
                        "ca_certificate_file": "/var/run/certs/ca_certificates.pem",
                        "refresh_interval": "60s"
                    }
                }
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(config.certificate_providers.len(), 1);

        let plugin = &config.certificate_providers["google_cloud_private_spiffe"];
        assert_eq!(plugin.plugin_name, "file_watcher");
        assert_eq!(
            plugin.config["certificate_file"],
            "/var/run/certs/certificates.pem"
        );
        assert_eq!(
            plugin.config["ca_certificate_file"],
            "/var/run/certs/ca_certificates.pem"
        );
        assert_eq!(plugin.config["refresh_interval"], "60s");
    }

    #[test]
    fn missing_certificate_providers_defaults_to_empty() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        assert!(config.certificate_providers.is_empty());
    }

    #[test]
    fn multiple_certificate_provider_instances() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}],
            "certificate_providers": {
                "identity": {
                    "plugin_name": "file_watcher",
                    "config": {
                        "certificate_file": "/certs/cert.pem",
                        "private_key_file": "/certs/key.pem"
                    }
                },
                "root_ca": {
                    "plugin_name": "file_watcher",
                    "config": {
                        "ca_certificate_file": "/certs/ca.pem"
                    }
                }
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(config.certificate_providers.len(), 2);
        assert!(config.certificate_providers.contains_key("identity"));
        assert!(config.certificate_providers.contains_key("root_ca"));
    }
}
