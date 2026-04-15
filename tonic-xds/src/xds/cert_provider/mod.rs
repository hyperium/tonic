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

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::xds::bootstrap::CertProviderPluginConfig;

/// Certificate material returned by a [`CertificateProvider`] plugin.
#[derive(Debug, Clone)]
pub(crate) struct CertificateData {
    /// PEM-encoded CA certificate(s) for validating peer certificates.
    pub root_certs: Option<Vec<u8>>,
    /// PEM-encoded identity certificate chain.
    pub identity_cert_chain: Option<Vec<u8>>,
    /// PEM-encoded private key for the identity certificate.
    pub identity_key: Option<Vec<u8>>,
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
