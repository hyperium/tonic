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

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rustls::pki_types::CertificateDer;

use crate::common::async_util::AbortOnDrop;
use crate::xds::cert_provider_config::FileWatcherConfig;

use super::{
    CertProviderError, CertificateData, CertificateProvider, Identity, default_crypto_provider,
};

/// Plugin name used in the bootstrap `certificate_providers` JSON.
pub(crate) const PLUGIN_NAME: &str = "file_watcher";

/// Refresh interval used when `FileWatcherConfig::refresh_interval` is unset.
/// Matches grpc-go's `defaultCertRefreshDuration`-equivalent for proxyless gRPC.
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(600);

/// A certificate provider that reads PEM files from disk.
///
/// On construction, reads all configured files synchronously and spawns a
/// background task that re-reads them on `config.refresh_interval`.
/// Read failures during refresh are logged, and the previously cached snapshot
/// is kept.
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
            "file_watcher cert refresh failed; keeping last successfully read data",
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
/// CA roots and identity material are read as raw PEM bytes; parsing is left to
/// the consumer. This function enforces cert/key pairing, which is shared by
/// A29 and A65, and the A29 file-watcher requirement that at least one
/// certificate file is configured. A65 empty configs bypass the file watcher
/// and use system roots directly.
fn read_certificate_data(config: &FileWatcherConfig) -> Result<CertificateData, CertProviderError> {
    let roots = config
        .ca_certificate_file
        .as_deref()
        .map(read_file)
        .transpose()?;

    let identity = match (&config.certificate_file, &config.private_key_file) {
        (Some(cert_path), Some(key_path)) => Some(read_identity(cert_path, key_path)?),
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

/// Read an identity, rejecting a certificate and private key that do not form
/// a usable pair.
///
/// The cert and the key live in two files, so a rotation that rewrites them
/// one at a time can be observed half-applied: a new cert read alongside a
/// stale key, or the reverse. Caching that pairing would break every
/// connection built from it until the next successful refresh, so it is
/// caught here and the caller keeps the previous snapshot instead.
///
/// Mirrors grpc-go's `file_watcher` provider, which discards an update whose
/// cert and key fail to form a valid pair.
fn read_identity(cert_path: &Path, key_path: &Path) -> Result<Identity, CertProviderError> {
    let cert_chain = read_file(cert_path)?;
    let key = read_file(key_path)?;

    validate_key_pair(&cert_chain, &key).map_err(|reason| {
        CertProviderError::InvalidIdentityPair {
            cert_path: cert_path.display().to_string(),
            key_path: key_path.display().to_string(),
            reason,
        }
    })?;

    Ok(Identity::new(cert_chain, key))
}

/// Check that `key_pem` is the private key for the leaf certificate in
/// `cert_pem`.
///
/// Delegates to rustls, which compares the `SubjectPublicKeyInfo` of the two
/// halves. Keys whose public half cannot be derived report
/// [`InconsistentKeys::Unknown`] and are accepted rather than rejected —
/// unverifiable is not the same as mismatched, and this is the same call
/// tonic makes when the material eventually reaches the TLS stack.
///
/// [`InconsistentKeys::Unknown`]: rustls::InconsistentKeys::Unknown
fn validate_key_pair(cert_pem: &[u8], key_pem: &[u8]) -> Result<(), String> {
    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut std::io::Cursor::new(cert_pem))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("failed to parse certificate PEM: {e}"))?;
    if cert_chain.is_empty() {
        return Err("no certificates found in certificate file".to_string());
    }

    let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_pem))
        .map_err(|e| format!("failed to parse private key PEM: {e}"))?
        .ok_or_else(|| "no private key found in private key file".to_string())?;

    rustls::sign::CertifiedKey::from_der(cert_chain, key, &default_crypto_provider())
        .map(|_| ())
        .map_err(|e| e.to_string())
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

    /// Generate a self-signed CA cert in PEM form, suitable for parsing into
    /// a [`RootCertStore`]. Returns the raw PEM bytes.
    fn gen_ca_pem() -> Vec<u8> {
        let mut params = rcgen::CertificateParams::new(vec!["test-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        cert.pem().into_bytes()
    }

    /// Generate a self-signed identity as `(cert_pem, key_pem)`. The two halves
    /// are a matching key pair, so they pass [`validate_key_pair`].
    fn gen_identity_pem() -> (Vec<u8>, Vec<u8>) {
        let params = rcgen::CertificateParams::new(vec!["test-leaf".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
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
        let ca_pem = gen_ca_pem();
        let ca_file = write_temp_file(&ca_pem);

        let provider =
            FileWatcherProvider::new(make_config(ca_file.path().to_str(), None, None)).unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::RootsOnly { .. }));
        assert_eq!(data.roots().unwrap(), ca_pem.as_slice());
        assert!(data.identity().is_none());
    }

    #[tokio::test]
    async fn reads_identity_cert_and_key() {
        let (cert_pem, key_pem) = gen_identity_pem();
        let cert_file = write_temp_file(&cert_pem);
        let key_file = write_temp_file(&key_pem);

        let provider = FileWatcherProvider::new(make_config(
            None,
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::IdentityOnly { .. }));
        let identity = data.identity().unwrap();
        assert_eq!(identity.cert_chain(), cert_pem.as_slice());
        assert_eq!(identity.key(), key_pem.as_slice());
        assert!(data.roots().is_none());
    }

    #[tokio::test]
    async fn reads_all_files() {
        let ca_pem = gen_ca_pem();
        let ca_file = write_temp_file(&ca_pem);
        let (cert_pem, key_pem) = gen_identity_pem();
        let cert_file = write_temp_file(&cert_pem);
        let key_file = write_temp_file(&key_pem);

        let provider = FileWatcherProvider::new(make_config(
            ca_file.path().to_str(),
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap();
        let data = provider.fetch().unwrap();

        assert!(matches!(*data, CertificateData::Both { .. }));
        assert_eq!(data.roots().unwrap(), ca_pem.as_slice());
        let identity = data.identity().unwrap();
        assert_eq!(identity.cert_chain(), cert_pem.as_slice());
        assert_eq!(identity.key(), key_pem.as_slice());
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
            "expected cache to keep last successfully read data on failure",
        );
    }

    #[test]
    fn mismatched_cert_and_key_are_rejected() {
        // Simulates a rotation observed half-applied: the cert has been
        // replaced but the key file still holds the previous key.
        let (cert_pem, _) = gen_identity_pem();
        let (_, other_key_pem) = gen_identity_pem();
        let cert_file = write_temp_file(&cert_pem);
        let key_file = write_temp_file(&other_key_pem);

        let err = read_certificate_data(&make_config(
            None,
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap_err();

        let CertProviderError::InvalidIdentityPair {
            cert_path,
            key_path,
            ..
        } = &err
        else {
            panic!("expected InvalidIdentityPair, got {err:?}");
        };
        assert_eq!(cert_path.as_str(), cert_file.path().to_str().unwrap());
        assert_eq!(key_path.as_str(), key_file.path().to_str().unwrap());
    }

    #[test]
    fn unparseable_identity_is_rejected() {
        let cert_file = write_temp_file(b"not a certificate");
        let key_file = write_temp_file(b"not a private key");

        let err = read_certificate_data(&make_config(
            None,
            cert_file.path().to_str(),
            key_file.path().to_str(),
        ))
        .unwrap_err();

        assert!(
            matches!(err, CertProviderError::InvalidIdentityPair { .. }),
            "expected InvalidIdentityPair, got {err:?}",
        );
    }

    /// A rotation that replaces the cert but not yet the key must not evict
    /// the last good snapshot — the torn pair would fail every handshake
    /// built from it until the following refresh.
    #[test]
    fn refresh_once_keeps_old_data_on_torn_rotation() {
        let (cert_pem, key_pem) = gen_identity_pem();
        let cert_file = write_temp_file(&cert_pem);
        let key_file = write_temp_file(&key_pem);
        let config = make_config(None, cert_file.path().to_str(), key_file.path().to_str());
        let cached = ArcSwap::from_pointee(read_certificate_data(&config).unwrap());
        let initial = cached.load_full();

        // Write only the new cert; the key file still holds the old key.
        let (rotated_cert_pem, rotated_key_pem) = gen_identity_pem();
        std::fs::write(cert_file.path(), &rotated_cert_pem).unwrap();
        refresh_once(&config, &cached);
        assert!(
            Arc::ptr_eq(&initial, &cached.load_full()),
            "expected the half-rotated pair to be discarded",
        );

        // Once the key catches up the pair is consistent again.
        std::fs::write(key_file.path(), &rotated_key_pem).unwrap();
        refresh_once(&config, &cached);
        let after = cached.load_full();
        assert_eq!(after.identity().unwrap().cert_chain(), rotated_cert_pem);
        assert_eq!(after.identity().unwrap().key(), rotated_key_pem);
    }

    /// A failed refresh keeps the cached data and waits out the full interval
    /// rather than retrying early.
    #[tokio::test(start_paused = true)]
    async fn failed_refresh_keeps_cached_data_until_the_next_interval() {
        let ca_file = write_temp_file(&gen_ca_pem());
        let ca_path = ca_file.path().to_path_buf();
        let mut config = make_config(ca_path.to_str(), None, None);
        config.refresh_interval = Some(Duration::from_secs(600));

        let cached = Arc::new(ArcSwap::from_pointee(
            read_certificate_data(&config).unwrap(),
        ));
        let initial = cached.load_full();
        let _task = AbortOnDrop(tokio::spawn(refresh_loop(config, Arc::clone(&cached))));

        // Break the file, then let the first scheduled refresh fail.
        std::fs::remove_file(&ca_path).unwrap();
        tokio::time::sleep(Duration::from_secs(601)).await;
        assert!(
            Arc::ptr_eq(&initial, &cached.load_full()),
            "failed refresh must not evict the cached data",
        );

        // Restore it. Without retry, the loop must still be waiting out the
        // rest of the interval.
        std::fs::write(&ca_path, gen_ca_pem()).unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(
            Arc::ptr_eq(&initial, &cached.load_full()),
            "no retry is implemented, so a restored file must not be picked up early",
        );

        // It is picked up at the next scheduled refresh.
        tokio::time::sleep(Duration::from_secs(541)).await;
        assert!(
            !Arc::ptr_eq(&initial, &cached.load_full()),
            "expected the next scheduled refresh to reload the restored file",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn successful_refresh_waits_for_the_full_interval() {
        let ca_file = write_temp_file(&gen_ca_pem());
        let mut config = make_config(ca_file.path().to_str(), None, None);
        config.refresh_interval = Some(Duration::from_secs(600));

        let cached = Arc::new(ArcSwap::from_pointee(
            read_certificate_data(&config).unwrap(),
        ));
        let initial = cached.load_full();
        let _task = AbortOnDrop(tokio::spawn(refresh_loop(config, Arc::clone(&cached))));

        // Well short of the interval: the loop must still be waiting.
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert!(
            Arc::ptr_eq(&initial, &cached.load_full()),
            "refresh must not run before the configured interval elapses",
        );

        tokio::time::sleep(Duration::from_secs(301)).await;
        assert!(
            !Arc::ptr_eq(&initial, &cached.load_full()),
            "refresh must run once the configured interval elapses",
        );
    }

    #[tokio::test]
    async fn registry_integration() {
        use crate::xds::bootstrap::CertProviderPluginConfig;
        use crate::xds::cert_provider::CertProviderRegistry;
        use std::collections::HashMap;

        let ca_pem = gen_ca_pem();
        let ca_file = write_temp_file(&ca_pem);

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

        let registry = CertProviderRegistry::from_bootstrap(&configs, HashMap::new()).unwrap();
        assert!(registry.get("my_certs").is_some());
        assert!(registry.get("other").is_none());

        let provider = registry.get("my_certs").unwrap();
        let data = provider.fetch().unwrap();
        assert_eq!(data.roots().unwrap(), ca_pem.as_slice());
    }
}
