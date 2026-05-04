// TODO: remove once A29 data plane TLS consumes all types.
#![allow(dead_code)]
//! Certificate provider plugin framework for gRFC A29.
//!
//! The xDS control plane references certificate providers by instance name
//! (via [`CertificateProviderPluginInstance`]). Each instance maps to a plugin
//! implementation configured in the bootstrap `certificate_providers` field.
//!
//! gRPC currently supports one built-in plugin: [`file_watcher`].
//!
//! [`CertificateProviderPluginInstance`]: https://github.com/envoyproxy/envoy/blob/main/api/envoy/extensions/transport_sockets/tls/v3/common.proto

pub(crate) mod file_watcher;
#[cfg(any(feature = "tls-ring", feature = "tls-aws-lc"))]
pub(crate) mod verifier;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::xds::bootstrap::CertProviderPluginConfig;

/// PEM-encoded identity (a cert chain paired with its private key).
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub(crate) cert_chain: Vec<u8>,
    pub(crate) key: Vec<u8>,
}

/// Certificate material returned by a [`CertificateProvider`] plugin.
///
/// The variants encode two invariants from gRFC A29 and A65 at the type level:
///
/// 1. **Cert/key pairing** (A65): identity cert and private key are paired or
///    absent — never one without the other. Guaranteed by [`Identity`].
/// 2. **At least one present** (A65, for `file_watcher`): at least one of
///    CA roots or identity must be set. Guaranteed by the absence of a
///    `Neither` variant — every value carries roots, identity, or both.
///
/// Spec references:
/// - A29: <https://github.com/grpc/proposal/blob/master/A29-xds-tls-security.md>
/// - A65: <https://github.com/grpc/proposal/blob/master/A65-xds-mtls-creds-in-bootstrap.md>
///   ("in the file-watcher certificate provider, at least one of the
///   `certificate_file` or `ca_certificate_file` fields must be specified")
#[derive(Debug, Clone)]
pub(crate) enum CertificateData {
    /// CA trust bundle only — used by TLS clients that don't present an
    /// identity.
    RootsOnly { roots: Vec<u8> },
    /// Identity only — used by TLS servers that don't validate peers
    /// (non-mTLS). Peer validation falls back to system roots at the
    /// consumer layer if needed.
    IdentityOnly { identity: Identity },
    /// Both roots and identity — used for mTLS on either end.
    Both { roots: Vec<u8>, identity: Identity },
}

impl CertificateData {
    /// PEM-encoded CA trust bundle, if present.
    pub(crate) fn roots(&self) -> Option<&[u8]> {
        match self {
            Self::RootsOnly { roots } | Self::Both { roots, .. } => Some(roots),
            Self::IdentityOnly { .. } => None,
        }
    }

    /// Identity cert chain and private key, if present.
    pub(crate) fn identity(&self) -> Option<&Identity> {
        match self {
            Self::IdentityOnly { identity } | Self::Both { identity, .. } => Some(identity),
            Self::RootsOnly { .. } => None,
        }
    }
}

/// Errors from certificate provider operations.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CertProviderError {
    #[error("failed to read certificate file '{path}': {source}")]
    FileRead {
        path: String,
        source: std::io::Error,
    },
    #[error("unknown certificate provider plugin: {0}")]
    UnknownPlugin(String),
    #[error("invalid config for plugin '{plugin}': {source}")]
    InvalidPluginConfig {
        plugin: String,
        source: serde_json::Error,
    },
    #[error(
        "invalid file_watcher config: 'certificate_file' and 'private_key_file' must both be \
         set or both be unset"
    )]
    UnpairedCertKey,
    #[error(
        "invalid file_watcher config: at least one of 'certificate_file' or \
         'ca_certificate_file' must be specified"
    )]
    EmptyConfig,
}

/// A certificate provider plugin.
///
/// Implementations obtain certificates from some source (local files, remote CA,
/// etc.) and deliver them to consumers. Providers cache their last successful
/// result and may refresh periodically.
pub(crate) trait CertificateProvider: Send + Sync {
    /// Fetch the current certificate data.
    ///
    /// Returns the most recently cached certificate material. This is called
    /// each time a new TLS connection is established. Returns an `Arc` to
    /// avoid deep-cloning certificate bytes on every call.
    fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError>;
}

/// Registry of certificate provider instances built from the bootstrap config.
///
/// Maps instance names to their provider implementations. Used during CDS
/// validation to verify that referenced instances exist, and at connection
/// time to fetch certificate material.
pub(crate) struct CertProviderRegistry {
    providers: HashMap<String, Arc<dyn CertificateProvider>>,
}

impl std::fmt::Debug for CertProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CertProviderRegistry {
    /// Build a registry from the bootstrap `certificate_providers` map.
    ///
    /// Dispatches on `plugin_name` and deserializes the opaque `config`
    /// into the appropriate plugin-specific type. Unknown plugin names
    /// are rejected here.
    pub(crate) fn from_bootstrap(
        configs: &HashMap<String, CertProviderPluginConfig>,
    ) -> Result<Self, CertProviderError> {
        let mut providers: HashMap<String, Arc<dyn CertificateProvider>> =
            HashMap::with_capacity(configs.len());

        for (instance_name, entry) in configs {
            let provider = Self::create_provider(entry)?;
            providers.insert(instance_name.clone(), provider);
        }

        Ok(Self { providers })
    }

    fn create_provider(
        entry: &CertProviderPluginConfig,
    ) -> Result<Arc<dyn CertificateProvider>, CertProviderError> {
        match entry.plugin_name.as_str() {
            file_watcher::PLUGIN_NAME => {
                let config =
                    file_watcher::FileWatcherConfig::deserialize(&entry.config).map_err(|e| {
                        CertProviderError::InvalidPluginConfig {
                            plugin: entry.plugin_name.clone(),
                            source: e,
                        }
                    })?;
                Ok(Arc::new(file_watcher::FileWatcherProvider::new(config)?))
            }
            other => Err(CertProviderError::UnknownPlugin(other.to_string())),
        }
    }

    /// Look up a provider instance by name.
    pub(crate) fn get(&self, instance_name: &str) -> Option<&Arc<dyn CertificateProvider>> {
        self.providers.get(instance_name)
    }

    /// Returns `true` if the given instance name is configured.
    pub(crate) fn contains(&self, instance_name: &str) -> bool {
        self.providers.contains_key(instance_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_bootstrap_creates_empty_registry() {
        let configs = HashMap::new();
        let registry = CertProviderRegistry::from_bootstrap(&configs).unwrap();
        assert!(registry.get("anything").is_none());
    }

    #[test]
    fn unknown_plugin_rejected_at_registry_build() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000"}],
            "certificate_providers": {
                "test": {
                    "plugin_name": "unknown_plugin",
                    "config": {}
                }
            }
        }"#;
        let config = crate::xds::bootstrap::BootstrapConfig::from_json(json).unwrap();
        let err = CertProviderRegistry::from_bootstrap(&config.certificate_providers);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("unknown_plugin"));
    }

    #[test]
    fn contains_returns_false_for_missing_instance() {
        let registry = CertProviderRegistry::from_bootstrap(&HashMap::new()).unwrap();
        assert!(!registry.contains("nonexistent"));
    }
}
