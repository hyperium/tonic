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
use serde::Deserialize;

use super::{CertProviderError, CertificateData, CertificateProvider};

/// Plugin name used in the bootstrap `certificate_providers` JSON.
pub(crate) const PLUGIN_NAME: &str = "file_watcher";

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
/// On construction, reads all configured files and caches the results.
/// The `fetch()` method returns the cached data.
// TODO(PR3/A29): Spawn a background task that calls `refresh()` on a timer
// driven by `config.refresh_interval` (default 600s). The task should be
// started in `new()` and cancelled on drop (e.g., via a JoinHandle +
// AbortHandle or a CancellationToken).
pub(crate) struct FileWatcherProvider {
    config: FileWatcherConfig,
    cached: ArcSwap<CertificateData>,
}

impl FileWatcherProvider {
    /// Create a new provider from a parsed `FileWatcherConfig`.
    pub(crate) fn new(config: FileWatcherConfig) -> Result<Self, CertProviderError> {
        let data = read_certificate_data(&config)?;

        Ok(Self {
            config,
            cached: ArcSwap::from_pointee(data),
        })
    }

    /// Re-read files from disk and update the cache.
    ///
    /// Returns `Ok(())` if the files were successfully read, or an error
    /// if any configured file could not be read. On error the cache retains
    /// the previous good data.
    #[allow(dead_code)] // Used when background refresh is added.
    pub(crate) fn refresh(&self) -> Result<(), CertProviderError> {
        let data = read_certificate_data(&self.config)?;
        self.cached.store(Arc::new(data));
        Ok(())
    }
}

impl CertificateProvider for FileWatcherProvider {
    fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
        Ok(self.cached.load_full())
    }
}

/// Read certificate data from the files specified in the config.
fn read_certificate_data(config: &FileWatcherConfig) -> Result<CertificateData, CertProviderError> {
    let root_certs = config
        .ca_certificate_file
        .as_deref()
        .map(read_file)
        .transpose()?;

    let identity_cert_chain = config
        .certificate_file
        .as_deref()
        .map(read_file)
        .transpose()?;

    let identity_key = config
        .private_key_file
        .as_deref()
        .map(read_file)
        .transpose()?;

    Ok(CertificateData {
        root_certs,
        identity_cert_chain,
        identity_key,
    })
}

fn read_file(path: &Path) -> Result<Vec<u8>, CertProviderError> {
    std::fs::read(path).map_err(|e| CertProviderError::FileRead {
        path: path.display().to_string(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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

    #[test]
    fn reads_ca_certificate() {
        let ca_file =
            write_temp_file(b"-----BEGIN CERTIFICATE-----\ntest-ca\n-----END CERTIFICATE-----\n");

        let provider =
            FileWatcherProvider::new(make_config(ca_file.path().to_str(), None, None)).unwrap();
        let data = provider.fetch().unwrap();

        assert!(
            data.root_certs
                .as_ref()
                .unwrap()
                .starts_with(b"-----BEGIN CERTIFICATE-----")
        );
        assert!(data.identity_cert_chain.is_none());
        assert!(data.identity_key.is_none());
    }

    #[test]
    fn reads_identity_cert_and_key() {
        let cert_file = write_temp_file(b"cert-chain-pem");
        let key_file = write_temp_file(b"private-key-pem");

        let provider = FileWatcherProvider::new(make_config(
            None,
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert_eq!(
            data.identity_cert_chain.as_deref(),
            Some(b"cert-chain-pem".as_slice())
        );
        assert_eq!(
            data.identity_key.as_deref(),
            Some(b"private-key-pem".as_slice())
        );
        assert!(data.root_certs.is_none());
    }

    #[test]
    fn reads_all_files() {
        let ca_file = write_temp_file(b"ca-pem");
        let cert_file = write_temp_file(b"cert-pem");
        let key_file = write_temp_file(b"key-pem");

        let provider = FileWatcherProvider::new(make_config(
            ca_file.path().to_str(),
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert_eq!(data.root_certs.as_deref(), Some(b"ca-pem".as_slice()));
        assert_eq!(
            data.identity_cert_chain.as_deref(),
            Some(b"cert-pem".as_slice())
        );
        assert_eq!(data.identity_key.as_deref(), Some(b"key-pem".as_slice()));
    }

    #[test]
    fn empty_config_returns_empty_data() {
        let provider = FileWatcherProvider::new(make_config(None, None, None)).unwrap();
        let data = provider.fetch().unwrap();

        assert!(data.root_certs.is_none());
        assert!(data.identity_cert_chain.is_none());
        assert!(data.identity_key.is_none());
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
    fn refresh_updates_cached_data() {
        let mut ca_file = NamedTempFile::new().unwrap();
        ca_file.write_all(b"old-ca").unwrap();

        let provider =
            FileWatcherProvider::new(make_config(ca_file.path().to_str(), None, None)).unwrap();
        assert_eq!(
            provider.fetch().unwrap().root_certs.as_deref(),
            Some(b"old-ca".as_slice())
        );

        std::fs::write(ca_file.path(), b"new-ca").unwrap();
        provider.refresh().unwrap();
        assert_eq!(
            provider.fetch().unwrap().root_certs.as_deref(),
            Some(b"new-ca".as_slice())
        );
    }

    #[test]
    fn refresh_keeps_old_data_on_failure() {
        let ca_file = write_temp_file(b"good-ca");
        let path = ca_file.path().to_str().unwrap().to_string();

        let provider = FileWatcherProvider::new(make_config(Some(&path), None, None)).unwrap();
        assert_eq!(
            provider.fetch().unwrap().root_certs.as_deref(),
            Some(b"good-ca".as_slice())
        );

        // Delete the file — refresh should fail.
        drop(ca_file);
        assert!(provider.refresh().is_err());

        // Cached data should still be the old value.
        assert_eq!(
            provider.fetch().unwrap().root_certs.as_deref(),
            Some(b"good-ca".as_slice())
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

    #[test]
    fn registry_integration() {
        use crate::xds::bootstrap::CertProviderPluginConfig;
        use crate::xds::cert_provider::CertProviderRegistry;
        use std::collections::HashMap;

        let ca_file = write_temp_file(b"ca-data");

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
        assert!(registry.contains("my_certs"));
        assert!(!registry.contains("other"));

        let provider = registry.get("my_certs").unwrap();
        let data = provider.fetch().unwrap();
        assert_eq!(data.root_certs.as_deref(), Some(b"ca-data".as_slice()));
    }
}
