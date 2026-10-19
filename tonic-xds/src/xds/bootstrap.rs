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

//! xDS bootstrap configuration.
//!
//! Parses the bootstrap JSON from `GRPC_XDS_BOOTSTRAP` (file path) or
//! `GRPC_XDS_BOOTSTRAP_CONFIG` (inline JSON) environment variables,
//! per gRFC A27.

use std::collections::HashMap;

use serde::Deserialize;
use xds_client::message::{Locality, MetadataValue, Node};

use crate::xds::cert_provider_config::{FileWatcherConfig, TlsChannelCredentials};

/// Environment variable pointing to a bootstrap JSON file path.
const ENV_BOOTSTRAP_FILE: &str = "GRPC_XDS_BOOTSTRAP";
/// Environment variable containing inline bootstrap JSON.
const ENV_BOOTSTRAP_CONFIG: &str = "GRPC_XDS_BOOTSTRAP_CONFIG";
/// `Node.user_agent_name`, which gRFC A27 has the client populate rather than
/// the bootstrap. The "gRPC <language>" prefix matches other gRPC xDS clients.
const USER_AGENT_NAME: &str = "gRPC Rust tonic-xds";

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
/// let json = r#"{
///   "xds_servers": [{
///     "server_uri": "xds.example.com:443",
///     "channel_creds": [{"type": "tls"}]
///   }]
/// }"#;
/// let config = BootstrapConfig::from_json(json).unwrap();
/// ```
///
/// # Inspecting
///
/// The fields are private so new bootstrap keys can be added without breaking
/// changes. The accessors below report what a loaded config will act on.
///
/// ```rust
/// use tonic_xds::BootstrapConfig;
///
/// let json = r#"{
///   "xds_servers": [{
///     "server_uri": "xds.example.com:443",
///     "channel_creds": [{"type": "tls"}]
///   }],
///   "node": {"id": "node-1"}
/// }"#;
/// let config = BootstrapConfig::from_json(json).unwrap();
/// assert_eq!(config.server_uri(), "xds.example.com:443");
/// assert_eq!(config.node_id(), "node-1");
/// assert!(config.use_tls());
/// ```
///
/// # Building programmatically
///
/// [`BootstrapConfig::builder`] constructs a config from typed values,
/// without needing a JSON string. It covers the
/// same bootstrap keys [`from_json`] acts on, per gRFC A27.
///
/// ```rust
/// use tonic_xds::{BootstrapConfig, ChannelCredentialType};
///
/// let config = BootstrapConfig::builder("xds.example.com:443")
///     .channel_creds([ChannelCredentialType::Tls])
///     .node_id("node-1")
///     .build()
///     .unwrap();
///
/// assert_eq!(config.server_uri(), "xds.example.com:443");
/// assert!(config.use_tls());
/// ```
///
/// [`from_env`]: BootstrapConfig::from_env
/// [`from_json`]: BootstrapConfig::from_json
/// [gRFC A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "BootstrapConfigDe")]
#[non_exhaustive]
pub struct BootstrapConfig {
    /// xDS management servers to connect to.
    pub(crate) xds_servers: Vec<XdsServerConfig>,
    /// Node identity sent to the xDS server.
    pub(crate) node: NodeConfig,
    /// Certificate provider plugin instances, keyed by instance name.
    ///
    /// Referenced by [`CertificateProviderPluginInstance`] in CDS/LDS
    /// `UpstreamTlsContext` / `DownstreamTlsContext` resources.
    /// See gRFC A29 for details.
    ///
    /// [`CertificateProviderPluginInstance`]: https://github.com/envoyproxy/envoy/blob/main/api/envoy/extensions/transport_sockets/tls/v3/common.proto
    // Consumed by `CertProviderRegistry::from_bootstrap` only under TLS
    // features; parsed regardless so non-TLS builds accept the same JSON.
    #[cfg_attr(not(feature = "_tls-any"), allow(dead_code))]
    pub(crate) certificate_providers: HashMap<String, CertProviderPluginConfig>,
}

/// Wire form of [`BootstrapConfig`].
///
/// [`BootstrapConfig`] deserializes through this type via `serde(try_from)`,
/// so a config a caller obtains by embedding it in their own config struct
/// goes through [`validate`](BootstrapConfig::validate) too.
#[derive(Deserialize)]
pub(crate) struct BootstrapConfigDe {
    xds_servers: Vec<XdsServerConfig>,
    #[serde(default)]
    node: NodeConfig,
    #[serde(default)]
    certificate_providers: HashMap<String, CertProviderPluginConfig>,
}

impl TryFrom<BootstrapConfigDe> for BootstrapConfig {
    type Error = BootstrapError;

    fn try_from(de: BootstrapConfigDe) -> Result<Self, Self::Error> {
        let config = Self {
            xds_servers: de.xds_servers,
            node: de.node,
            certificate_providers: de.certificate_providers,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Configuration for a single xDS management server.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct XdsServerConfig {
    /// URI of the xDS server (e.g., `"xds.example.com:443"`).
    pub server_uri: String,
    /// Ordered list of channel credentials. The client uses the first supported type.
    ///
    /// gRFC A27 requires the key. `#[serde(default)]` routes a missing list to
    /// [`BootstrapConfig::validate`], which reports it with the same message it
    /// gives an empty or unknown-only one.
    #[serde(default)]
    pub channel_creds: Vec<ChannelCredentialConfig>,
    /// Server features (e.g., `["xds_v3"]`).
    #[serde(default)]
    #[allow(dead_code)]
    // Parsed for completeness; used when server feature negotiation is added.
    pub server_features: Vec<String>,
}

impl XdsServerConfig {
    /// First credential type this client supports, per gRFC A27.
    ///
    /// Skips unknown types so a bootstrap written for a newer client still
    /// resolves. Returns `None` for a config still awaiting
    /// [`BootstrapConfig::validate`], which requires a match.
    fn selected_credential(&self) -> Option<&ChannelCredentialConfig> {
        self.channel_creds
            .iter()
            .find(|credential| credential.cred_type.is_supported())
    }
}

/// A channel credential entry from the bootstrap config.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct ChannelCredentialConfig {
    /// Credential type (e.g., `"insecure"`, `"tls"`, `"google_default"`).
    #[serde(rename = "type")]
    pub cred_type: ChannelCredentialType,
    /// Credential-specific configuration.
    #[serde(default = "empty_json_object")]
    pub config: serde_json::Value,
}

fn empty_json_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl ChannelCredentialConfig {
    fn tls_config(&self, server_index: usize) -> Result<Option<FileWatcherConfig>, BootstrapError> {
        if !matches!(self.cred_type, ChannelCredentialType::Tls) {
            return Ok(None);
        }

        let config: FileWatcherConfig =
            serde_json::from_value(self.config.clone()).map_err(|error| {
                BootstrapError::Validation(format!(
                    "xds_servers[{server_index}].channel_creds tls config is invalid: {error}"
                ))
            })?;
        if !config.has_paired_identity_files() {
            return Err(BootstrapError::Validation(format!(
                "xds_servers[{server_index}].channel_creds tls config must set \
                 'certificate_file' and 'private_key_file' together"
            )));
        }
        Ok(Some(config))
    }
}

/// Channel credential type offered to the xDS management server.
///
/// The client uses the first type it supports, per [gRFC A27]. This is the
/// bootstrap's `xds_servers[].channel_creds[].type`, and is what
/// [`BootstrapConfigBuilder::channel_creds`] accepts.
///
/// [gRFC A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelCredentialType {
    /// Plaintext connection to the xDS server.
    Insecure,
    /// TLS with the platform's default trust roots.
    Tls,
    /// Google default credentials (ALTS or TLS plus call credentials).
    ///
    /// Parsed for forward compatibility, but not selected because tonic-xds
    /// does not yet provide Google default channel credentials.
    GoogleDefault,
    /// A credential type this client does not implement.
    ///
    /// Produced when parsing a bootstrap written for a newer or different
    /// client; such entries are skipped when selecting a credential, so a
    /// bootstrap can list types this version does not know about.
    ///
    /// `#[non_exhaustive]` reserves the variant for the parser: downstream
    /// code can neither construct it nor match its payload. [`Deserialize`]
    /// still yields one for an unknown type, so
    /// [`BootstrapConfigBuilder::build`] rejects it.
    #[serde(untagged)]
    #[non_exhaustive]
    Unsupported(String),
}

impl ChannelCredentialType {
    fn is_supported(&self) -> bool {
        matches!(self, Self::Insecure | Self::Tls)
    }

    /// The bootstrap JSON spelling, so errors name what the caller wrote.
    fn as_json_name(&self) -> &str {
        match self {
            Self::Insecure => "insecure",
            Self::Tls => "tls",
            Self::GoogleDefault => "google_default",
            Self::Unsupported(name) => name,
        }
    }

    /// Whether this supported type expects a TLS handshake.
    fn is_tls(&self) -> bool {
        matches!(self, Self::Tls)
    }
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
// In non-TLS builds `cert_provider` is gated out, so nothing reads these
// fields after serde populates them.
#[cfg_attr(not(feature = "_tls-any"), allow(dead_code))]
pub(crate) struct CertProviderPluginConfig {
    pub plugin_name: String,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Node identity configuration from bootstrap JSON.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
    /// Accepts any JSON value (nested objects, arrays, numbers, bools, null)
    /// per the proto3 JSON mapping for `google.protobuf.Struct`. Some control
    /// planes vary the served config based on metadata — e.g. Istio's istiod
    /// gates proxyless gRPC config behind `GENERATOR = "grpc"`.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Convert a `serde_json::Value` to the codec-agnostic [`MetadataValue`].
fn json_to_metadata(value: serde_json::Value) -> Result<MetadataValue, BootstrapError> {
    Ok(match value {
        serde_json::Value::Null => MetadataValue::Null,
        serde_json::Value::Bool(b) => MetadataValue::Bool(b),
        serde_json::Value::Number(n) => MetadataValue::Number(n.as_f64().ok_or_else(|| {
            BootstrapError::Validation(format!("metadata number {n} not representable as f64"))
        })?),
        serde_json::Value::String(s) => MetadataValue::String(s),
        serde_json::Value::Array(arr) => MetadataValue::Array(
            arr.into_iter()
                .map(json_to_metadata)
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(obj) => MetadataValue::Object(
            obj.into_iter()
                .map(|(k, v)| json_to_metadata(v).map(|mv| (k, mv)))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Locality configuration from bootstrap JSON.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
#[non_exhaustive]
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
    /// A value passed to [`BootstrapConfigBuilder`] could not be serialized to
    /// JSON.
    ///
    /// Setters take `impl Serialize` for free-form fields such as
    /// `node.metadata`; [`build`](BootstrapConfigBuilder::build) reports the
    /// first one that failed.
    #[error("failed to serialize bootstrap {field}: {source}")]
    Serialization {
        /// Field whose value could not be serialized (e.g. `node.metadata`).
        field: &'static str,
        /// Underlying serialization error.
        source: serde_json::Error,
    },
}

impl BootstrapConfig {
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
        // The wire form reports validation failures as
        // `BootstrapError::Validation`; `Self` reports them as a serde error.
        let de: BootstrapConfigDe = serde_json::from_str(json)?;
        Self::try_from(de)
    }

    /// Checks the invariants every constructor upholds.
    ///
    /// Runs on both construction paths — `TryFrom<BootstrapConfigDe>`, which
    /// covers [`from_json`], [`from_env`] and `Deserialize`, and
    /// [`BootstrapConfigBuilder::build`] — so [`server_uri`] can index
    /// `xds_servers` directly and [`use_tls`] reports a credential the client
    /// actually selected.
    ///
    /// [`from_json`]: BootstrapConfig::from_json
    /// [`from_env`]: BootstrapConfig::from_env
    /// [`server_uri`]: BootstrapConfig::server_uri
    /// [`use_tls`]: BootstrapConfig::use_tls
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
            // gRFC A27 requires `channel_creds` to name at least one type the
            // client supports. Enforcing it makes the control plane's security
            // level an explicit choice, so a missing, empty or unknown-only
            // list fails here.
            let Some(selected) = server.selected_credential() else {
                let listed = server
                    .channel_creds
                    .iter()
                    .map(|c| c.cred_type.as_json_name())
                    .collect::<Vec<_>>();
                let found = if listed.is_empty() {
                    "it is missing or empty".to_string()
                } else {
                    format!("found only [{}]", listed.join(", "))
                };
                return Err(BootstrapError::Validation(format!(
                    "xds_servers[{i}].channel_creds must list at least one credential type this \
                     client supports (insecure, tls); {found}"
                )));
            };
            selected.tls_config(i)?;
            Self::validate_uri_scheme(i, &server.server_uri, &selected.cred_type)?;
        }
        Ok(())
    }

    /// Requires the `server_uri` and the selected credential to agree on TLS.
    ///
    /// The scheme drives the handshake: the transport upgrades a scheme-less
    /// URI to `https` when the bootstrap selects TLS, keeps any explicit scheme
    /// as written, and handshakes for `https` alone. TLS therefore holds on
    /// exactly those two forms. Any other scheme — `http://`, `unix://`,
    /// anything unrecognised — connects in the clear while the client reports a
    /// secure channel and attaches call credentials to it.
    ///
    /// `https://` with `insecure` states the same conflict from the other side
    /// and fails at connect time, so this catches it up front.
    fn validate_uri_scheme(
        index: usize,
        server_uri: &str,
        credential: &ChannelCredentialType,
    ) -> Result<(), BootstrapError> {
        // Mirrors the transport, which parses with `http::Uri` as well: it
        // reports no scheme for the bare authority form (`xds.example.com:443`)
        // it upgrades, and errors on the forms it hands to its own connectors
        // (`unix://...`). A parse error therefore counts as plaintext.
        let parsed = server_uri.parse::<http::Uri>();
        let upgraded_to_tls = matches!(&parsed, Ok(uri) if uri.scheme_str().is_none());
        let is_https = matches!(&parsed, Ok(uri) if uri.scheme_str() == Some("https"));
        let cred = credential.as_json_name();

        if credential.is_tls() && !(upgraded_to_tls || is_https) {
            return Err(BootstrapError::Validation(format!(
                "xds_servers[{index}].server_uri '{server_uri}' connects in plaintext but \
                 channel_creds selects '{cred}'; use an 'https://' or scheme-less URI, or \
                 select 'insecure'"
            )));
        }
        if !credential.is_tls() && is_https {
            return Err(BootstrapError::Validation(format!(
                "xds_servers[{index}].server_uri '{server_uri}' requires TLS but channel_creds \
                 selects '{cred}'; list 'tls' or drop the 'https://' scheme"
            )));
        }
        Ok(())
    }

    /// Returns the URI of the xDS server this config connects to.
    ///
    /// Only the first entry is used; further `xds_servers` are parsed but not
    /// yet connected to.
    pub fn server_uri(&self) -> &str {
        self.xds_servers
            .first()
            .map(|s| s.server_uri.as_str())
            .expect("xds_servers validated non-empty")
    }

    /// Returns the node identifier presented to the xDS server.
    ///
    /// Empty when the bootstrap omits `node.id`.
    pub fn node_id(&self) -> &str {
        &self.node.id
    }

    /// Select the first supported channel credential type from the first server's config.
    ///
    /// Validation guarantees one exists, so a validated config always returns
    /// `Some`.
    pub(crate) fn selected_credential(&self) -> Option<&ChannelCredentialType> {
        self.xds_servers
            .first()?
            .selected_credential()
            .map(|credential| &credential.cred_type)
    }

    #[cfg_attr(not(feature = "_tls-any"), allow(dead_code))]
    pub(crate) fn tls_config(&self) -> Result<Option<FileWatcherConfig>, BootstrapError> {
        let Some(server) = self.xds_servers.first() else {
            return Ok(None);
        };
        match server.selected_credential() {
            Some(credential) => credential.tls_config(0),
            None => Ok(None),
        }
    }

    /// Returns `true` if the connection to the xDS server uses TLS.
    ///
    /// Every constructor requires the `server_uri` scheme to agree with the
    /// selected credential, so a `true` here means the transport really does
    /// handshake.
    pub fn use_tls(&self) -> bool {
        self.selected_credential()
            .is_some_and(ChannelCredentialType::is_tls)
    }

    /// Starts building a bootstrap config for the given xDS server URI.
    ///
    /// Use this when the process already holds the configuration, so it can
    /// build the config directly. [`from_json`] remains the full-fidelity
    /// path and the only one that accepts a gRFC A27 document verbatim.
    ///
    /// ```rust
    /// use tonic_xds::{BootstrapConfig, ChannelCredentialType};
    ///
    /// let config = BootstrapConfig::builder("xds.example.com:443")
    ///     .channel_creds([ChannelCredentialType::Tls])
    ///     .node_id("my-node")
    ///     .build()
    ///     .unwrap();
    ///
    /// assert_eq!(config.server_uri(), "xds.example.com:443");
    /// assert_eq!(config.node_id(), "my-node");
    /// assert!(config.use_tls());
    /// ```
    ///
    /// [`from_json`]: BootstrapConfig::from_json
    pub fn builder(server_uri: impl Into<String>) -> BootstrapConfigBuilder {
        BootstrapConfigBuilder::new(server_uri)
    }
}

/// Builds a [`BootstrapConfig`] without going through JSON.
///
/// Created by [`BootstrapConfig::builder`], which takes the required server URI
/// up front. gRFC A27 also requires [`channel_creds`], which [`build`] checks.
/// Every other setter is optional.
///
/// The builder covers the bootstrap keys the client acts on. A bootstrap that
/// needs keys outside that set — additional `xds_servers` entries, which are
/// parsed but not yet connected to — must still come from [`from_json`].
///
/// ```rust
/// use tonic_xds::{BootstrapConfig, ChannelCredentialType};
///
/// let config = BootstrapConfig::builder("xds.example.com:443")
///     .channel_creds([ChannelCredentialType::Tls, ChannelCredentialType::Insecure])
///     .node_id("projects/123/nodes/456")
///     .node_cluster("my-cluster")
///     .node_locality("us-east1", "us-east1-b", "rack1")
///     .node_metadata("GENERATOR", "grpc")
///     .build()
///     .unwrap();
///
/// assert!(config.use_tls());
/// ```
///
/// [`channel_creds`]: BootstrapConfigBuilder::channel_creds
/// [`build`]: BootstrapConfigBuilder::build
/// [`from_json`]: BootstrapConfig::from_json
#[derive(Debug)]
pub struct BootstrapConfigBuilder {
    server_uri: String,
    channel_creds: Vec<ChannelCredentialConfig>,
    node_id: String,
    node_cluster: Option<String>,
    node_locality: Option<LocalityConfig>,
    node_metadata: HashMap<String, serde_json::Value>,
    certificate_providers: HashMap<String, CertProviderPluginConfig>,
    error: Option<BootstrapError>,
}

impl BootstrapConfigBuilder {
    fn new(server_uri: impl Into<String>) -> Self {
        Self {
            server_uri: server_uri.into(),
            channel_creds: Vec::new(),
            node_id: String::new(),
            node_cluster: None,
            node_locality: None,
            node_metadata: HashMap::new(),
            certificate_providers: HashMap::new(),
            error: None,
        }
    }

    /// Sets the credentials offered to the xDS server, in preference order.
    ///
    /// Replaces any previously set credentials. Calling this is required: gRFC
    /// A27 asks for at least one type the client supports, and [`build`]
    /// enforces it. Pass [`ChannelCredentialType::Tls`] for a secured server,
    /// or [`ChannelCredentialType::Insecure`] to state plaintext explicitly.
    ///
    /// [`build`]: BootstrapConfigBuilder::build
    #[must_use]
    pub fn channel_creds(mut self, creds: impl IntoIterator<Item = ChannelCredentialType>) -> Self {
        self.channel_creds = creds
            .into_iter()
            .map(|cred_type| ChannelCredentialConfig {
                cred_type,
                config: empty_json_object(),
            })
            .collect();
        self
    }

    /// Configures the first `tls` channel credential with gRFC A65 file
    /// settings.
    ///
    /// If [`channel_creds`](Self::channel_creds) does not already contain
    /// [`ChannelCredentialType::Tls`], a TLS entry is appended. Credential
    /// preference still follows list order, so configure the desired ordering
    /// with `channel_creds` first.
    #[must_use]
    pub fn tls_channel_credentials(mut self, config: TlsChannelCredentials) -> Self {
        match serde_json::to_value(config) {
            Ok(config) => {
                if let Some(credential) = self
                    .channel_creds
                    .iter_mut()
                    .find(|credential| matches!(credential.cred_type, ChannelCredentialType::Tls))
                {
                    credential.config = config;
                } else {
                    self.channel_creds.push(ChannelCredentialConfig {
                        cred_type: ChannelCredentialType::Tls,
                        config,
                    });
                }
            }
            Err(source) => self.record_error("xds_servers[].channel_creds[].config", source),
        }
        self
    }

    /// Sets the opaque node identifier presented to the xDS server.
    #[must_use]
    pub fn node_id(mut self, id: impl Into<String>) -> Self {
        self.node_id = id.into();
        self
    }

    /// Sets the cluster the node belongs to.
    #[must_use]
    pub fn node_cluster(mut self, cluster: impl Into<String>) -> Self {
        self.node_cluster = Some(cluster.into());
        self
    }

    /// Sets the locality the node is running in.
    #[must_use]
    pub fn node_locality(
        mut self,
        region: impl Into<String>,
        zone: impl Into<String>,
        sub_zone: impl Into<String>,
    ) -> Self {
        self.node_locality = Some(LocalityConfig {
            region: region.into(),
            zone: zone.into(),
            sub_zone: sub_zone.into(),
        });
        self
    }

    /// Adds one free-form node metadata entry, replacing any entry with the
    /// same key.
    ///
    /// Accepts anything [`Serialize`], so a string, a number, or a nested
    /// struct all work. Some control planes vary the served config based on
    /// metadata — e.g. Istio's istiod gates proxyless gRPC config behind
    /// `GENERATOR = "grpc"`.
    ///
    /// [`build`] reports a value with no JSON form as
    /// [`BootstrapError::Serialization`].
    ///
    /// [`Serialize`]: serde::Serialize
    /// [`build`]: BootstrapConfigBuilder::build
    #[must_use]
    pub fn node_metadata(mut self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        let key = key.into();
        match serde_json::to_value(value) {
            Ok(value) => {
                self.node_metadata.insert(key, value);
            }
            Err(source) => self.record_error("node.metadata", source),
        }
        self
    }

    /// Registers a certificate provider instance, replacing any instance with
    /// the same name.
    ///
    /// `instance_name` is what CDS/LDS resources reference by
    /// `CertificateProviderPluginInstance`; `config` is the opaque
    /// plugin-specific blob, accepted as anything [`Serialize`]. See [gRFC A29].
    ///
    /// [`build`] reports a config with no JSON form as
    /// [`BootstrapError::Serialization`].
    ///
    /// [`Serialize`]: serde::Serialize
    /// [`build`]: BootstrapConfigBuilder::build
    /// [gRFC A29]: https://github.com/grpc/proposal/blob/master/A29-xds-tls-security.md
    #[must_use]
    pub fn certificate_provider(
        mut self,
        instance_name: impl Into<String>,
        plugin_name: impl Into<String>,
        config: impl serde::Serialize,
    ) -> Self {
        let instance_name = instance_name.into();
        match serde_json::to_value(config) {
            Ok(config) => {
                self.certificate_providers.insert(
                    instance_name,
                    CertProviderPluginConfig {
                        plugin_name: plugin_name.into(),
                        config,
                    },
                );
            }
            Err(source) => self.record_error("certificate_providers", source),
        }
        self
    }

    /// Keeps the first error so the chain can continue and report at `build`.
    fn record_error(&mut self, field: &'static str, source: serde_json::Error) {
        if self.error.is_none() {
            self.error = Some(BootstrapError::Serialization { field, source });
        }
    }

    /// Validates the accumulated settings and returns the config.
    ///
    /// # Errors
    ///
    /// - [`BootstrapError::Serialization`] if a metadata or certificate
    ///   provider value failed to serialize to JSON; the first failing setter
    ///   wins.
    /// - [`BootstrapError::Validation`] if the credentials include
    ///   [`ChannelCredentialType::Unsupported`], or if the config fails the
    ///   same validation JSON-parsed configs go through: an empty server URI,
    ///   no supported [`channel_creds`], or a URI scheme that disagrees with
    ///   the selected credential.
    ///
    /// [`channel_creds`]: BootstrapConfigBuilder::channel_creds
    pub fn build(self) -> Result<BootstrapConfig, BootstrapError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        // Parsing skips `Unsupported` for forward compatibility; building
        // treats it as a caller mistake. `Deserialize` is the only source of
        // one, since the constructor is sealed.
        if let Some(ChannelCredentialType::Unsupported(name)) = self
            .channel_creds
            .iter()
            .map(|c| &c.cred_type)
            .find(|t| matches!(t, ChannelCredentialType::Unsupported(_)))
        {
            return Err(BootstrapError::Validation(format!(
                "channel credential type '{name}' is not supported by this client"
            )));
        }
        let config = BootstrapConfig {
            xds_servers: vec![XdsServerConfig {
                server_uri: self.server_uri,
                channel_creds: self.channel_creds,
                // Server feature negotiation is still to come, so the builder
                // omits a setter.
                server_features: Vec::new(),
            }],
            node: NodeConfig {
                id: self.node_id,
                cluster: self.node_cluster,
                locality: self.node_locality,
                metadata: self.node_metadata,
            },
            certificate_providers: self.certificate_providers,
        };
        config.validate()?;
        Ok(config)
    }
}

impl TryFrom<NodeConfig> for Node {
    type Error = BootstrapError;

    fn try_from(config: NodeConfig) -> Result<Self, Self::Error> {
        let mut node = Node::new(USER_AGENT_NAME, env!("CARGO_PKG_VERSION"));

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
            let metadata: HashMap<String, MetadataValue> = config
                .metadata
                .into_iter()
                .map(|(k, v)| json_to_metadata(v).map(|mv| (k, mv)))
                .collect::<Result<_, _>>()?;
            node = node.with_metadata(metadata);
        }

        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_json() -> &'static str {
        r#"{
            "xds_servers": [{
                "server_uri": "xds.example.com:443",
                "channel_creds": [{"type": "insecure"}]
            }],
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
            ChannelCredentialType::GoogleDefault
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
        let node = Node::try_from(config.node).unwrap();
        assert_eq!(node.id.as_deref(), Some("projects/123/nodes/456"));
        assert_eq!(node.cluster.as_deref(), Some("test-cluster"));
        assert_eq!(node.user_agent_name, "gRPC Rust tonic-xds");

        let locality = node.locality.unwrap();
        assert_eq!(locality.region, "us-east1");
        assert_eq!(locality.zone, "us-east1-b");
        assert_eq!(locality.sub_zone, "rack1");
    }

    #[test]
    fn node_from_minimal_config() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        let node = Node::try_from(config.node).unwrap();
        assert_eq!(node.id.as_deref(), Some("test-node"));
        assert!(node.cluster.is_none());
        assert!(node.locality.is_none());
    }

    #[test]
    fn selected_credential_first_supported_wins() {
        let config = BootstrapConfig::from_json(full_json()).unwrap();
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
    fn unknown_only_channel_creds_fail() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "some_future_type"}]
            }],
            "node": {"id": "n1"}
        }"#;
        // gRFC A27 requires a type the client supports; an unknown-only list
        // otherwise falls through to a plaintext control-plane connection.
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(matches!(err, BootstrapError::Validation(_)));
        assert!(
            err.to_string()
                .contains("channel_creds must list at least one")
        );
        assert!(err.to_string().contains("some_future_type"));
    }

    #[test]
    fn missing_channel_creds_fail() {
        let json = r#"{
            "xds_servers": [{"server_uri": "xds.example.com:443"}],
            "node": {"id": "n1"}
        }"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("is missing or empty"));
    }

    #[test]
    fn empty_channel_creds_fail() {
        let json = r#"{
            "xds_servers": [{"server_uri": "localhost:5000", "channel_creds": []}],
            "node": {"id": "n1"}
        }"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("is missing or empty"));
    }

    #[test]
    fn a_uri_that_cannot_carry_tls_with_tls_creds_fails() {
        // The transport upgrades the scheme-less form alone and handshakes for
        // `https` alone, so each of these connects in the clear while `use_tls`
        // reports a secure channel.
        for uri in [
            "http://xds.example.com:443",
            // Parses, and tonic treats an unrecognised scheme as plaintext.
            "foo://xds.example.com:443",
            "dns://xds.example.com:443",
            // Fails `http::Uri`; the transport routes it to a plaintext connector.
            "unix:///etc/istio/proxy/XDS",
            "unix:/etc/istio/proxy/XDS",
        ] {
            let json = format!(
                r#"{{
                    "xds_servers": [{{
                        "server_uri": "{uri}",
                        "channel_creds": [{{"type": "tls"}}]
                    }}]
                }}"#
            );
            let err = BootstrapConfig::from_json(&json).unwrap_err();
            assert!(err.to_string().contains("connects in plaintext"), "{uri}");
        }
    }

    #[test]
    fn tls_uri_with_insecure_creds_fails() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "https://xds.example.com:443",
                "channel_creds": [{"type": "insecure"}]
            }]
        }"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("requires TLS but channel_creds"));
    }

    #[test]
    fn explicit_schemes_matching_their_creds_are_accepted() {
        for (uri, cred, tls) in [
            ("https://xds.example.com:443", "tls", true),
            ("http://localhost:18000", "insecure", false),
            // No scheme: the transport picks one from the credential.
            ("xds.example.com:443", "tls", true),
            // Fails `http::Uri`; the transport routes it to its UDS connector.
            ("unix:///etc/istio/proxy/XDS", "insecure", false),
            // An unrecognised scheme stays plaintext, which `insecure` agrees with.
            ("dns://xds.example.com:443", "insecure", false),
        ] {
            let json = format!(
                r#"{{"xds_servers": [{{"server_uri": "{uri}", "channel_creds": [{{"type": "{cred}"}}]}}]}}"#
            );
            let config = BootstrapConfig::from_json(&json)
                .unwrap_or_else(|e| panic!("{uri} with {cred} should parse: {e}"));
            assert_eq!(config.use_tls(), tls, "{uri} with {cred}");
        }
    }

    #[test]
    fn a_later_server_is_validated_too() {
        let json = r#"{
            "xds_servers": [
                {"server_uri": "primary:443", "channel_creds": [{"type": "tls"}]},
                {"server_uri": "fallback:443"}
            ]
        }"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("xds_servers[1].channel_creds"));
    }

    #[test]
    fn builder_rejects_a_plaintext_uri_with_tls_creds() {
        let err = BootstrapConfig::builder("http://xds.example.com:443")
            .channel_creds([ChannelCredentialType::Tls])
            .build()
            .unwrap_err();
        assert!(err.to_string().contains("connects in plaintext"));
    }

    #[test]
    fn builder_requires_channel_creds() {
        let err = BootstrapConfig::builder("xds.example.com:443")
            .node_id("n1")
            .build()
            .unwrap_err();
        assert!(err.to_string().contains("is missing or empty"));
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
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }]
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let node = Node::try_from(config.node).unwrap();
        assert!(node.id.is_none());
    }

    #[test]
    fn public_accessors_report_what_the_client_will_use() {
        let json = r#"{
            "xds_servers": [
                {"server_uri": "primary:443", "channel_creds": [{"type": "tls"}]},
                {"server_uri": "fallback:443", "channel_creds": [{"type": "tls"}]}
            ],
            "node": {"id": "node-1"}
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();

        assert_eq!(config.server_uri(), "primary:443");
        assert_eq!(config.node_id(), "node-1");
        assert!(config.use_tls());
    }

    #[test]
    fn public_accessors_report_absent_optional_fields() {
        let json = r#"{"xds_servers": [{"server_uri": "localhost:5000", "channel_creds": [{"type": "insecure"}]}]}"#;
        let config = BootstrapConfig::from_json(json).unwrap();

        assert_eq!(config.node_id(), "");
        assert!(!config.use_tls());
    }

    #[test]
    fn equal_configs_compare_equal_regardless_of_json_formatting() {
        let compact = r#"{"xds_servers":[{"server_uri":"xds:443","channel_creds":[{"type":"insecure"}]}],"node":{"id":"n1"}}"#;
        let spaced = r#"{
            "node": {"id": "n1"},
            "xds_servers": [{
                "server_uri": "xds:443",
                "channel_creds": [{"type": "insecure"}]
            }]
        }"#;

        assert_eq!(
            BootstrapConfig::from_json(compact).unwrap(),
            BootstrapConfig::from_json(spaced).unwrap(),
        );
    }

    #[test]
    fn misplaced_keys_are_dropped_and_compare_unequal() {
        let creds = r#""channel_creds":[{"type":"insecure"}]"#;
        let intended = format!(
            r#"{{"xds_servers":[{{"server_uri":"xds:443",{creds}}}],"node":{{"id":"n1"}}}}"#
        );
        let misplaced =
            format!(r#"{{"xds_servers":[{{"server_uri":"xds:443",{creds}}}],"node_id":"n1"}}"#);

        let misparsed = BootstrapConfig::from_json(&misplaced).unwrap();
        assert_eq!(misparsed.node_id(), "");
        assert_ne!(BootstrapConfig::from_json(&intended).unwrap(), misparsed);
    }

    #[test]
    fn builder_matches_the_equivalent_json() {
        let built = BootstrapConfig::builder("xds.example.com:443")
            .channel_creds([
                ChannelCredentialType::GoogleDefault,
                ChannelCredentialType::Tls,
                ChannelCredentialType::Insecure,
            ])
            .node_id("projects/123/nodes/456")
            .node_cluster("test-cluster")
            .node_locality("us-east1", "us-east1-b", "rack1")
            .build()
            .unwrap();

        // `full_json` also carries `server_features`; the client ignores it,
        // so the builder omits it.
        let parsed = BootstrapConfig::from_json(full_json()).unwrap();
        assert_eq!(parsed.xds_servers[0].server_features, vec!["xds_v3"]);
        assert!(built.xds_servers[0].server_features.is_empty());

        let mut parsed_without_features = parsed;
        parsed_without_features.xds_servers[0]
            .server_features
            .clear();
        assert_eq!(parsed_without_features, built);
    }

    #[test]
    fn builder_defaults_omit_optional_fields() {
        let built = BootstrapConfig::builder("xds.example.com:443")
            .channel_creds([ChannelCredentialType::Insecure])
            .node_id("test-node")
            .build()
            .unwrap();

        assert_eq!(BootstrapConfig::from_json(minimal_json()).unwrap(), built);
        assert!(!built.use_tls());
    }

    #[test]
    fn builder_carries_metadata_and_cert_providers() {
        let built = BootstrapConfig::builder("localhost:5000")
            .channel_creds([ChannelCredentialType::Insecure])
            .node_metadata("GENERATOR", "grpc")
            .certificate_provider(
                "google_cloud_private_spiffe",
                "file_watcher",
                serde_json::json!({"certificate_file": "/etc/certs/cert.pem"}),
            )
            .build()
            .unwrap();

        let equivalent = BootstrapConfig::from_json(
            r#"{
                "xds_servers": [{
                    "server_uri": "localhost:5000",
                    "channel_creds": [{"type": "insecure"}]
                }],
                "node": {"metadata": {"GENERATOR": "grpc"}},
                "certificate_providers": {
                    "google_cloud_private_spiffe": {
                        "plugin_name": "file_watcher",
                        "config": {"certificate_file": "/etc/certs/cert.pem"}
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(equivalent, built);
    }

    #[test]
    fn builder_runs_the_same_validation_as_json() {
        let err = BootstrapConfig::builder("").build().unwrap_err();
        assert!(matches!(err, BootstrapError::Validation(_)));
        assert!(err.to_string().contains("server_uri must not be empty"));
    }

    #[test]
    fn builder_reports_unrepresentable_metadata_at_build() {
        // JSON object keys must be strings, so a tuple-keyed map has no JSON
        // form and `build` reports it.
        let unrepresentable = HashMap::from([((1u8, 2u8), "v")]);
        let err = BootstrapConfig::builder("xds:443")
            .node_metadata("bad", unrepresentable)
            .build()
            .unwrap_err();

        assert!(matches!(
            err,
            BootstrapError::Serialization {
                field: "node.metadata",
                ..
            }
        ));
    }

    #[test]
    fn builder_reports_only_the_first_serialization_error() {
        let unrepresentable = HashMap::from([((1u8, 2u8), "v")]);
        let err = BootstrapConfig::builder("xds:443")
            .node_metadata("bad", unrepresentable.clone())
            .certificate_provider("bad", "file_watcher", unrepresentable)
            .build()
            .unwrap_err();

        assert!(matches!(
            err,
            BootstrapError::Serialization {
                field: "node.metadata",
                ..
            }
        ));
    }

    #[test]
    fn parsed_unsupported_creds_are_skipped_but_rejected_by_the_builder() {
        // Parsing keeps forward compatibility: an unknown type is skipped and
        // the next supported one wins.
        let parsed = BootstrapConfig::from_json(
            r#"{
                "xds_servers": [{
                    "server_uri": "xds:443",
                    "channel_creds": [{"type": "future_creds"}, {"type": "tls"}]
                }]
            }"#,
        )
        .unwrap();
        assert!(parsed.use_tls());

        // Building states what this client should use, so `build` rejects the
        // same value parsing skips. `Deserialize` is the only way to name one.
        let err = BootstrapConfig::builder("xds:443")
            .channel_creds(
                parsed.xds_servers[0]
                    .channel_creds
                    .iter()
                    .map(|c| c.cred_type.clone()),
            )
            .build()
            .unwrap_err();

        assert!(matches!(err, BootstrapError::Validation(_)));
        assert!(err.to_string().contains("future_creds"));
    }

    #[test]
    fn deserializing_a_config_directly_runs_validation() {
        // `BootstrapConfig` derives `Deserialize` so callers can embed it in
        // their own config structs; that path validates too, keeping
        // `server_uri` total.
        let err =
            serde_json::from_str::<BootstrapConfig>(r#"{"xds_servers": []}"#).expect_err("empty");
        assert!(err.to_string().contains("xds_servers must not be empty"));

        let ok = serde_json::from_str::<BootstrapConfig>(
            r#"{"xds_servers": [{"server_uri": "xds:443", "channel_creds": [{"type": "tls"}]}]}"#,
        )
        .unwrap();
        assert_eq!(ok.server_uri(), "xds:443");
    }

    #[test]
    fn builder_setters_replace_rather_than_accumulate() {
        let built = BootstrapConfig::builder("xds:443")
            .channel_creds([ChannelCredentialType::Insecure])
            .channel_creds([ChannelCredentialType::Tls])
            .node_metadata("k", "first")
            .node_metadata("k", "second")
            .build()
            .unwrap();

        assert!(built.use_tls());
        assert_eq!(built.node.metadata["k"], serde_json::json!("second"));
    }

    #[test]
    fn parse_node_metadata() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
            "node": {
                "id": "n1",
                "metadata": {
                    "GENERATOR": "grpc",
                    "PILOT_VERSION": "1.20"
                }
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(
            config.node.metadata.get("GENERATOR").unwrap(),
            &serde_json::Value::String("grpc".to_string())
        );
        assert_eq!(
            config.node.metadata.get("PILOT_VERSION").unwrap(),
            &serde_json::Value::String("1.20".to_string())
        );
    }

    #[test]
    fn node_from_config_propagates_metadata() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
            "node": {
                "id": "n1",
                "metadata": {"GENERATOR": "grpc"}
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let node = Node::try_from(config.node).unwrap();
        assert_eq!(
            node.metadata.get("GENERATOR").unwrap(),
            &MetadataValue::String("grpc".to_string())
        );
    }

    #[test]
    fn parse_istio_style_metadata() {
        // Real-world Istio bootstrap shape: nested objects, numbers, arrays.
        let json = r#"{
            "xds_servers": [{
                "server_uri": "unix:///etc/istio/proxy/XDS",
                "channel_creds": [{"type": "insecure"}]
            }],
            "node": {
                "id": "sidecar~10.0.0.1~pod.ns~ns.svc.cluster.local",
                "metadata": {
                    "GENERATOR": "grpc",
                    "ANNOTATIONS": {
                        "inject.istio.io/templates": "grpc-agent",
                        "istio.io/rev": "default"
                    },
                    "CLUSTER_ID": "Kubernetes",
                    "ENVOY_PROMETHEUS_PORT": 15090,
                    "PILOT_SAN": ["istiod.istio-system.svc"]
                }
            }
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let node = Node::try_from(config.node).unwrap();

        assert_eq!(
            node.metadata.get("GENERATOR").unwrap(),
            &MetadataValue::String("grpc".to_string())
        );
        assert_eq!(
            node.metadata.get("ENVOY_PROMETHEUS_PORT").unwrap(),
            &MetadataValue::Number(15090.0)
        );

        match node.metadata.get("ANNOTATIONS").unwrap() {
            MetadataValue::Object(fields) => {
                assert_eq!(
                    fields.get("istio.io/rev").unwrap(),
                    &MetadataValue::String("default".to_string())
                );
            }
            other => panic!("expected Object, got {other:?}"),
        }

        match node.metadata.get("PILOT_SAN").unwrap() {
            MetadataValue::Array(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(
                    &items[0],
                    &MetadataValue::String("istiod.istio-system.svc".to_string())
                );
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn missing_metadata_defaults_to_empty() {
        let config = BootstrapConfig::from_json(minimal_json()).unwrap();
        assert!(config.node.metadata.is_empty());
        let node = Node::try_from(config.node).unwrap();
        assert!(node.metadata.is_empty());
    }

    #[test]
    fn parse_certificate_providers() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
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
    fn unsupported_google_default_falls_back_to_tls() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "xds.example.com:443",
                "channel_creds": [
                    {"type": "google_default"},
                    {"type": "tls"}
                ]
            }],
            "node": {"id": "n1"}
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        assert_eq!(
            config.xds_servers[0].channel_creds[0].cred_type,
            ChannelCredentialType::GoogleDefault
        );
        assert!(config.use_tls());
        assert_eq!(
            config.selected_credential(),
            Some(&ChannelCredentialType::Tls)
        );
    }

    #[test]
    fn google_default_without_a_supported_fallback_fails() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "xds.example.com:443",
                "channel_creds": [{"type": "google_default"}]
            }]
        }"#;
        let err = BootstrapConfig::from_json(json).unwrap_err();
        assert!(err.to_string().contains("found only [google_default]"));
    }

    #[test]
    fn parses_a65_tls_config() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "xds.example.com:443",
                "channel_creds": [{
                    "type": "tls",
                    "config": {
                        "ca_certificate_file": "/certs/ca.pem",
                        "certificate_file": "/certs/client.pem",
                        "private_key_file": "/certs/client.key",
                        "refresh_interval": "0.5s"
                    }
                }]
            }]
        }"#;
        let config = BootstrapConfig::from_json(json).unwrap();
        let tls = config.tls_config().unwrap().unwrap();
        assert_eq!(
            tls.ca_certificate_file.as_deref(),
            Some(std::path::Path::new("/certs/ca.pem"))
        );
        assert_eq!(
            tls.certificate_file.as_deref(),
            Some(std::path::Path::new("/certs/client.pem"))
        );
        assert_eq!(
            tls.private_key_file.as_deref(),
            Some(std::path::Path::new("/certs/client.key"))
        );
        assert_eq!(
            tls.refresh_interval,
            Some(std::time::Duration::from_millis(500))
        );
    }

    #[test]
    fn a65_tls_config_may_use_only_system_roots() {
        for credential in [r#"{"type": "tls"}"#, r#"{"type": "tls", "config": {}}"#] {
            let json = format!(
                r#"{{
                    "xds_servers": [{{
                        "server_uri": "xds.example.com:443",
                        "channel_creds": [{credential}]
                    }}]
                }}"#
            );
            let config = BootstrapConfig::from_json(&json).unwrap();
            assert!(
                !config
                    .tls_config()
                    .unwrap()
                    .unwrap()
                    .has_certificate_files()
            );
        }
    }

    #[test]
    fn a65_tls_config_requires_certificate_and_key_together() {
        for field in ["certificate_file", "private_key_file"] {
            let json = format!(
                r#"{{
                    "xds_servers": [{{
                        "server_uri": "xds.example.com:443",
                        "channel_creds": [{{
                            "type": "tls",
                            "config": {{"{field}": "/certs/file.pem"}}
                        }}]
                    }}]
                }}"#
            );
            let err = BootstrapConfig::from_json(&json).unwrap_err();
            assert!(
                err.to_string()
                    .contains("must set 'certificate_file' and 'private_key_file' together")
            );
        }
    }

    #[test]
    fn a65_tls_config_rejects_invalid_refresh_interval() {
        let err = BootstrapConfig::from_json(
            r#"{
                "xds_servers": [{
                    "server_uri": "xds.example.com:443",
                    "channel_creds": [{
                        "type": "tls",
                        "config": {"refresh_interval": "0s"}
                    }]
                }]
            }"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("must be greater than 0"));
    }

    #[test]
    fn builder_configures_a65_tls_credentials() {
        let built = BootstrapConfig::builder("xds.example.com:443")
            .channel_creds([
                ChannelCredentialType::GoogleDefault,
                ChannelCredentialType::Tls,
            ])
            .tls_channel_credentials(
                TlsChannelCredentials::new()
                    .with_ca_certificate_file("/certs/ca.pem")
                    .with_identity_files("/certs/client.pem", "/certs/client.key")
                    .with_refresh_interval(std::time::Duration::from_millis(500)),
            )
            .build()
            .unwrap();
        let parsed = BootstrapConfig::from_json(
            r#"{
                "xds_servers": [{
                    "server_uri": "xds.example.com:443",
                    "channel_creds": [
                        {"type": "google_default"},
                        {
                            "type": "tls",
                            "config": {
                                "ca_certificate_file": "/certs/ca.pem",
                                "certificate_file": "/certs/client.pem",
                                "private_key_file": "/certs/client.key",
                                "refresh_interval": "0.5s"
                            }
                        }
                    ]
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(built, parsed);
    }

    #[test]
    fn multiple_certificate_provider_instances() {
        let json = r#"{
            "xds_servers": [{
                "server_uri": "localhost:5000",
                "channel_creds": [{"type": "insecure"}]
            }],
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
