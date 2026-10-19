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
pub(crate) mod verifier;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::xds::bootstrap::CertProviderPluginConfig;
use crate::xds::cert_provider_config::FileWatcherConfig;

/// Resolve a [`rustls::crypto::CryptoProvider`]: prefer the process-installed
/// default, fall back to a feature-flagged provider. Mirrors tonic's
/// `transport::channel::service::tls` bootstrap so we make the same choice as
/// the rest of the channel stack.
pub(crate) fn default_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    if let Some(p) = rustls::crypto::CryptoProvider::get_default() {
        return p.clone();
    }
    #[cfg(feature = "tls-ring")]
    return Arc::new(rustls::crypto::ring::default_provider());
    #[cfg(all(not(feature = "tls-ring"), feature = "tls-aws-lc"))]
    return Arc::new(rustls::crypto::aws_lc_rs::default_provider());
}

/// PEM-encoded identity (a cert chain paired with its private key).
#[derive(Clone)]
pub struct Identity {
    cert_chain: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    /// Creates an identity from a PEM certificate chain and PEM private key.
    pub fn new(cert_chain: Vec<u8>, key: Vec<u8>) -> Self {
        Self { cert_chain, key }
    }

    /// PEM-encoded certificate chain.
    pub fn cert_chain(&self) -> &[u8] {
        &self.cert_chain
    }

    /// PEM-encoded private key.
    pub fn key(&self) -> &[u8] {
        &self.key
    }
}

// Manual `Debug` keeps the private key out of logs.
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field(
                "cert_chain",
                &format_args!("{} bytes", self.cert_chain.len()),
            )
            .field("key", &format_args!("<redacted>"))
            .finish()
    }
}

/// Certificate material returned by a [`CertificateProvider`] plugin.
///
/// Both the CA trust bundle and the identity are carried as PEM bytes; the
/// consumer parses them (e.g. into a rustls `RootCertStore`) at the point of
/// use.
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
pub enum CertificateData {
    /// CA trust bundle only — used by TLS clients that don't present an
    /// identity.
    RootsOnly {
        /// PEM-encoded CA trust anchors.
        roots: Vec<u8>,
    },
    /// Identity only — used by TLS servers that don't validate peers
    /// (non-mTLS). Peer validation falls back to system roots at the
    /// consumer layer if needed.
    IdentityOnly {
        /// Local certificate chain and private key.
        identity: Identity,
    },
    /// Both roots and identity — used for mTLS on either end.
    Both {
        /// PEM-encoded CA trust anchors.
        roots: Vec<u8>,
        /// Local certificate chain and private key.
        identity: Identity,
    },
}

impl CertificateData {
    /// PEM-encoded CA trust bundle, if present.
    pub(crate) fn roots(&self) -> Option<&[u8]> {
        match self {
            Self::RootsOnly { roots } | Self::Both { roots, .. } => Some(roots.as_slice()),
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
#[non_exhaustive]
pub enum CertProviderError {
    /// A certificate file could not be read.
    #[error("failed to read certificate file '{path}': {source}")]
    FileRead {
        /// Path that failed to read.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A certificate file was not valid PEM.
    #[error("failed to parse PEM in '{path}': {reason}")]
    PemParse {
        /// Path that failed to parse.
        path: String,
        /// Parse failure reason.
        reason: String,
    },
    /// The bootstrap named a plugin that is not built in.
    #[error("unknown certificate provider plugin: {0}")]
    UnknownPlugin(String),
    /// A plugin's bootstrap config failed to deserialize.
    #[error("invalid config for plugin '{plugin}': {source}")]
    InvalidPluginConfig {
        /// Plugin name.
        plugin: String,
        /// Underlying deserialization error.
        source: serde_json::Error,
    },
    /// `certificate_file` and `private_key_file` must both be set or unset.
    #[error(
        "invalid file_watcher config: 'certificate_file' and 'private_key_file' must both be \
         set or both be unset"
    )]
    UnpairedCertKey,
    /// The identity certificate and private key do not form a usable pair,
    /// either because they are not the same key pair or because one of them
    /// failed to parse.
    ///
    /// Could happen when the two files were read mid-rotation, with one
    /// already replaced, partially written, or not yet replaced. The snapshot
    /// is discarded instead of cached, so the last known-good material stays
    /// in use.
    #[error(
        "certificate '{cert_path}' and private key '{key_path}' are not a \
         valid pair (the files may have been read during a rotation): {reason}"
    )]
    InvalidIdentityPair {
        /// Path of the certificate chain file.
        cert_path: String,
        /// Path of the private key file.
        key_path: String,
        /// Why the pair was rejected.
        reason: String,
    },
    /// Neither an identity nor a CA bundle was configured.
    #[error(
        "invalid file_watcher config: at least one of 'certificate_file' or \
         'ca_certificate_file' must be specified"
    )]
    EmptyConfig,
    /// Catch-all for errors raised by out-of-crate provider implementations.
    #[error("certificate provider error: {0}")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

/// A certificate provider plugin.
///
/// Implementations obtain certificates from some source (local files, remote CA,
/// etc.) and deliver them to consumers. Providers cache their last successful
/// result and may refresh periodically.
pub trait CertificateProvider: Send + Sync {
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
    /// Builds a registry from the bootstrap `certificate_providers` map,
    /// dispatching on `plugin_name`, and merges in externally supplied provider
    /// instances that shadow bootstrap instances of the same name.
    pub(crate) fn from_bootstrap(
        configs: &HashMap<String, CertProviderPluginConfig>,
        injected: HashMap<String, Arc<dyn CertificateProvider>>,
    ) -> Result<Self, CertProviderError> {
        let mut providers: HashMap<String, Arc<dyn CertificateProvider>> =
            HashMap::with_capacity(configs.len() + injected.len());

        for (instance_name, entry) in configs {
            // Injected providers shadow bootstrap instances of the same name.
            if injected.contains_key(instance_name) {
                continue;
            }
            let provider = Self::create_provider(entry)?;
            providers.insert(instance_name.clone(), provider);
        }

        providers.extend(injected);

        Ok(Self { providers })
    }

    fn create_provider(
        entry: &CertProviderPluginConfig,
    ) -> Result<Arc<dyn CertificateProvider>, CertProviderError> {
        match entry.plugin_name.as_str() {
            file_watcher::PLUGIN_NAME => {
                let config = FileWatcherConfig::deserialize(&entry.config).map_err(|e| {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_bootstrap_creates_empty_registry() {
        let configs = HashMap::new();
        let registry = CertProviderRegistry::from_bootstrap(&configs, HashMap::new()).unwrap();
        assert!(registry.get("anything").is_none());
    }

    #[test]
    fn unknown_plugin_rejected_at_registry_build() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
            "certificate_providers": {
                "test": {
                    "plugin_name": "unknown_plugin",
                    "config": {}
                }
            }
        }"#;
        let config = crate::xds::bootstrap::BootstrapConfig::from_json(json).unwrap();
        let err =
            CertProviderRegistry::from_bootstrap(&config.certificate_providers, HashMap::new());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("unknown_plugin"));
    }

    #[test]
    fn get_returns_none_for_missing_instance() {
        let registry =
            CertProviderRegistry::from_bootstrap(&HashMap::new(), HashMap::new()).unwrap();
        assert!(registry.get("nonexistent").is_none());
    }

    struct StaticProvider(Arc<CertificateData>);

    impl CertificateProvider for StaticProvider {
        fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn injected_provider_is_resolvable_by_instance_name() {
        let provider: Arc<dyn CertificateProvider> =
            Arc::new(StaticProvider(Arc::new(CertificateData::RootsOnly {
                roots: Vec::new(),
            })));
        let mut injected: HashMap<String, Arc<dyn CertificateProvider>> = HashMap::new();
        injected.insert("dv".to_string(), provider);

        let registry = CertProviderRegistry::from_bootstrap(&HashMap::new(), injected).unwrap();

        assert!(registry.get("dv").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn injected_provider_overrides_bootstrap_instance_of_same_name() {
        // The bootstrap "shared" instance points at a missing file, so it would
        // fail to build; the injected "shared" must shadow it entirely.
        let mut configs = HashMap::new();
        configs.insert(
            "shared".to_string(),
            CertProviderPluginConfig {
                plugin_name: file_watcher::PLUGIN_NAME.to_string(),
                config: serde_json::json!({
                    "ca_certificate_file": "/nonexistent/ca.pem",
                }),
            },
        );

        let injected_data = Arc::new(CertificateData::IdentityOnly {
            identity: Identity::new(b"injected-cert".to_vec(), b"injected-key".to_vec()),
        });
        let mut injected: HashMap<String, Arc<dyn CertificateProvider>> = HashMap::new();
        injected.insert(
            "shared".to_string(),
            Arc::new(StaticProvider(injected_data)),
        );

        let registry = CertProviderRegistry::from_bootstrap(&configs, injected).unwrap();

        let data = registry.get("shared").unwrap().fetch().unwrap();
        assert!(
            matches!(*data, CertificateData::IdentityOnly { .. }),
            "expected the injected provider to win over the bootstrap instance",
        );
    }
}
