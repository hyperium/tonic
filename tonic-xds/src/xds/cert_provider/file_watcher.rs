//! `file_watcher` certificate provider plugin.
//!
//! Reads PEM-encoded certificates and keys from local files. This is the
//! only built-in certificate provider plugin per gRFC A29.
//!
//! # Bootstrap configuration
//!
//! ```json
//! {
//!   "plugin_name": "file_watcher",
//!   "config": {
//!     "certificate_file": "/path/to/cert.pem",
//!     "private_key_file": "/path/to/key.pem",
//!     "ca_certificate_file": "/path/to/ca.pem",
//!     "refresh_interval": "60s"
//!   }
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use serde::Deserialize;

use crate::common::async_util::AbortOnDrop;

use super::{CertProviderError, CertificateData, CertificateProvider, Identity};

/// Plugin name used in the bootstrap `certificate_providers` JSON.
pub(crate) const PLUGIN_NAME: &str = "file_watcher";

/// Refresh interval used when `FileWatcherConfig::refresh_interval` is unset.
/// Matches grpc-go's `defaultCertRefreshDuration`-equivalent for proxyless gRPC.
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(600);

/// Configuration for the `file_watcher` certificate provider.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FileWatcherConfig {
    /// Path to PEM X.509 identity certificate or certificate chain.
    #[serde(default)]
    pub certificate_file: Option<PathBuf>,
    /// Path to PEM PKCS private key.
    #[serde(default)]
    pub private_key_file: Option<PathBuf>,
    /// Path to PEM X.509 CA trust bundle (root certificates).
    #[serde(default)]
    pub ca_certificate_file: Option<PathBuf>,
    /// How often to re-read the files. Default: 600s.
    /// Parsed from protobuf JSON duration format (e.g., `"60s"`, `"0.5s"`).
    #[serde(default, deserialize_with = "deserialize_proto_duration")]
    pub refresh_interval: Option<Duration>,
}

/// Deserialize a protobuf JSON duration string (e.g., `"60s"`, `"0.5s"`) into a `Duration`.
fn deserialize_proto_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(s) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let num = s.strip_suffix('s').ok_or_else(|| {
        serde::de::Error::custom(format!("invalid duration '{s}': must end with 's'"))
    })?;
    let secs: f64 = num
        .parse()
        .map_err(|_| serde::de::Error::custom(format!("invalid duration number: '{num}'")))?;
    if secs < 0.0 {
        return Err(serde::de::Error::custom(format!(
            "invalid duration '{s}': must not be negative"
        )));
    }
    Ok(Some(Duration::from_secs_f64(secs)))
}

/// A certificate provider that reads PEM files from disk.
///
/// On construction, reads all configured files synchronously and spawns a
/// background task that re-reads them on `config.refresh_interval`.
/// Read or parse failures during refresh are logged;
/// the previously cached data is kept.
pub(crate) struct FileWatcherProvider {
    cached: Arc<ArcSwap<CertificateData>>,
    _refresh_task: AbortOnDrop,
}

impl FileWatcherProvider {
    /// Create a new provider from a parsed `FileWatcherConfig`.
    pub(crate) fn new(config: FileWatcherConfig) -> Result<Self, CertProviderError> {
        let data = read_certificate_data(&config)?;
        let cached = Arc::new(ArcSwap::from_pointee(data));
        let task = tokio::spawn(refresh_loop(config, Arc::clone(&cached)));
        Ok(Self {
            cached,
            _refresh_task: AbortOnDrop(task),
        })
    }
}

/// Background task: periodically re-read the configured files and update
/// the shared cache.
async fn refresh_loop(config: FileWatcherConfig, cached: Arc<ArcSwap<CertificateData>>) {
    let period = config.refresh_interval.unwrap_or(DEFAULT_REFRESH_INTERVAL);
    let mut ticker = tokio::time::interval(period);
    // `interval` fires immediately on the first `tick()`. The initial data was
    // already loaded synchronously in `new()`, so discard that first tick.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        refresh_once(&config, &cached);
    }
}

/// Re-read the configured files once and update the cache. On failure,
/// log and leave the cache unchanged.
fn refresh_once(config: &FileWatcherConfig, cached: &ArcSwap<CertificateData>) {
    match read_certificate_data(config) {
        Ok(data) => cached.store(Arc::new(data)),
        Err(e) => tracing::warn!(
            error = ?e,
            "file_watcher cert refresh failed; keeping last good data",
        ),
    }
}

impl CertificateProvider for FileWatcherProvider {
    fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
        Ok(self.cached.load_full())
    }
}

/// Read certificate data from the files specified in the config.
///
/// CA roots are parsed into [`Arc<RootCertStore>`] in this function — once per
/// refresh — so the verifier can use them directly on every TLS handshake
/// without re-parsing. Identity bytes are kept as PEM because
/// [`tonic::transport::Identity::from_pem`] is bytes-only on the consumer side.
///
/// This function is the single validation boundary between the permissive
/// JSON-parsed [`FileWatcherConfig`] and the invariant-enforcing
/// [`CertificateData`]. It checks both A65 rules:
/// - cert/key pairing (first match)
/// - at least one of identity/roots is set (second match)
fn read_certificate_data(config: &FileWatcherConfig) -> Result<CertificateData, CertProviderError> {
    let roots = config
        .ca_certificate_file
        .as_deref()
        .map(read_and_parse_roots)
        .transpose()?;

    let identity = match (&config.certificate_file, &config.private_key_file) {
        (Some(cert_path), Some(key_path)) => Some(Identity {
            cert_chain: read_file(cert_path)?,
            key: read_file(key_path)?,
        }),
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => return Err(CertProviderError::UnpairedCertKey),
    };

    match (roots, identity) {
        (Some(roots), Some(identity)) => Ok(CertificateData::Both { roots, identity }),
        (Some(roots), None) => Ok(CertificateData::RootsOnly { roots }),
        (None, Some(identity)) => Ok(CertificateData::IdentityOnly { identity }),
        (None, None) => Err(CertProviderError::EmptyConfig),
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, CertProviderError> {
    std::fs::read(path).map_err(|e| CertProviderError::FileRead {
        path: path.display().to_string(),
        source: e,
    })
}

fn read_and_parse_roots(path: &Path) -> Result<Arc<RootCertStore>, CertProviderError> {
    let pem = read_file(path)?;
    let mut reader = std::io::Cursor::new(&pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<_, _>>()
        .map_err(|e| CertProviderError::PemParse {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    let mut store = RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(CertProviderError::PemParse {
            path: path.display().to_string(),
            reason: "no usable certificates in PEM".into(),
        });
    }
    Ok(Arc::new(store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Generate a self-signed CA cert in PEM form, suitable for parsing into
    /// a [`RootCertStore`]. Returns the raw PEM bytes.
    fn gen_ca_pem() -> Vec<u8> {
        let mut params = rcgen::CertificateParams::new(vec!["test-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        cert.pem().into_bytes()
    }

    fn write_temp_file(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f
    }

    fn make_config(ca: Option<&str>, cert: Option<&str>, key: Option<&str>) -> FileWatcherConfig {
        FileWatcherConfig {
            certificate_file: cert.map(Into::into),
            private_key_file: key.map(Into::into),
            ca_certificate_file: ca.map(Into::into),
            refresh_interval: None,
        }
    }

    #[tokio::test]
    async fn reads_ca_certificate() {
        let ca_file = write_temp_file(&gen_ca_pem());

        let provider =
            FileWatcherProvider::new(make_config(ca_file.path().to_str(), None, None)).unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::RootsOnly { .. }));
        let roots = data.roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert!(data.identity().is_none());
    }

    #[tokio::test]
    async fn reads_identity_cert_and_key() {
        let cert_file = write_temp_file(b"cert-chain-pem");
        let key_file = write_temp_file(b"private-key-pem");

        let provider = FileWatcherProvider::new(make_config(
            None,
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::IdentityOnly { .. }));
        let identity = data.identity().unwrap();
        assert_eq!(identity.cert_chain.as_slice(), b"cert-chain-pem");
        assert_eq!(identity.key.as_slice(), b"private-key-pem");
        assert!(data.roots().is_none());
    }

    #[tokio::test]
    async fn reads_all_files() {
        let ca_file = write_temp_file(&gen_ca_pem());
        let cert_file = write_temp_file(b"cert-pem");
        let key_file = write_temp_file(b"key-pem");

        let provider = FileWatcherProvider::new(make_config(
            ca_file.path().to_str(),
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::Both { .. }));
        assert_eq!(data.roots().unwrap().len(), 1);
        let identity = data.identity().unwrap();
        assert_eq!(identity.cert_chain.as_slice(), b"cert-pem");
        assert_eq!(identity.key.as_slice(), b"key-pem");
    }

    #[test]
    fn empty_config_returns_error() {
        let err = FileWatcherProvider::new(make_config(None, None, None))
            .err()
            .unwrap();
        assert!(matches!(err, CertProviderError::EmptyConfig));
    }

    #[test]
    fn cert_without_key_returns_error() {
        let cert_file = write_temp_file(b"cert-pem");
        let err = FileWatcherProvider::new(make_config(None, cert_file.path().to_str(), None))
            .err()
            .unwrap();
        assert!(matches!(err, CertProviderError::UnpairedCertKey));
    }

    #[test]
    fn key_without_cert_returns_error() {
        let key_file = write_temp_file(b"key-pem");
        let err = FileWatcherProvider::new(make_config(None, None, key_file.path().to_str()))
            .err()
            .unwrap();
        assert!(matches!(err, CertProviderError::UnpairedCertKey));
    }

    #[test]
    fn missing_file_returns_error() {
        let result =
            FileWatcherProvider::new(make_config(Some("/nonexistent/path/ca.pem"), None, None));
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("/nonexistent/path/ca.pem")
        );
    }

    #[test]
    fn refresh_once_updates_cache() {
        let ca_file = write_temp_file(&gen_ca_pem());
        let config = make_config(ca_file.path().to_str(), None, None);
        let cached = ArcSwap::from_pointee(read_certificate_data(&config).unwrap());
        let initial = cached.load_full();

        std::fs::write(ca_file.path(), gen_ca_pem()).unwrap();
        refresh_once(&config, &cached);

        let after = cached.load_full();
        assert!(
            !Arc::ptr_eq(&initial, &after),
            "expected refresh_once to swap cached Arc",
        );
    }

    #[test]
    fn refresh_once_keeps_old_data_on_failure() {
        let ca_file = write_temp_file(&gen_ca_pem());
        let config = make_config(ca_file.path().to_str(), None, None);
        let cached = ArcSwap::from_pointee(read_certificate_data(&config).unwrap());
        let initial = cached.load_full();

        drop(ca_file);
        refresh_once(&config, &cached);

        let after = cached.load_full();
        assert!(
            Arc::ptr_eq(&initial, &after),
            "expected cache to keep last good data on failure",
        );
    }

    #[test]
    fn parse_refresh_interval_seconds() {
        let config: FileWatcherConfig =
            serde_json::from_value(serde_json::json!({"refresh_interval": "60s"})).unwrap();
        assert_eq!(config.refresh_interval, Some(Duration::from_secs(60)));
    }

    #[test]
    fn parse_refresh_interval_fractional() {
        let config: FileWatcherConfig =
            serde_json::from_value(serde_json::json!({"refresh_interval": "0.5s"})).unwrap();
        assert_eq!(config.refresh_interval, Some(Duration::from_millis(500)));
    }

    #[test]
    fn parse_refresh_interval_absent() {
        let config: FileWatcherConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(config.refresh_interval, None);
    }

    #[test]
    fn parse_refresh_interval_missing_suffix() {
        let err = serde_json::from_value::<FileWatcherConfig>(
            serde_json::json!({"refresh_interval": "60"}),
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("must end with 's'"));
    }

    #[test]
    fn parse_refresh_interval_not_a_number() {
        let err = serde_json::from_value::<FileWatcherConfig>(
            serde_json::json!({"refresh_interval": "60ms"}),
        );
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("invalid duration number")
        );
    }

    #[test]
    fn parse_refresh_interval_negative() {
        let err = serde_json::from_value::<FileWatcherConfig>(
            serde_json::json!({"refresh_interval": "-1s"}),
        );
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("must not be negative")
        );
    }

    #[tokio::test]
    async fn registry_integration() {
        use crate::xds::bootstrap::CertProviderPluginConfig;
        use crate::xds::cert_provider::CertProviderRegistry;
        use std::collections::HashMap;

        let ca_file = write_temp_file(&gen_ca_pem());

        let mut configs = HashMap::new();
        configs.insert(
            "my_certs".to_string(),
            CertProviderPluginConfig {
                plugin_name: "file_watcher".to_string(),
                config: serde_json::json!({
                    "ca_certificate_file": ca_file.path().to_str().unwrap(),
                }),
            },
        );

        let registry = CertProviderRegistry::from_bootstrap(&configs).unwrap();
        assert!(registry.get("my_certs").is_some());
        assert!(registry.get("other").is_none());

        let provider = registry.get("my_certs").unwrap();
        let data = provider.fetch().unwrap();
        assert_eq!(data.roots().unwrap().len(), 1);
    }
}
