/*
 *
 * Copyright 2026 gRPC authors.
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

use std::io::BufReader;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::PrivateKeyDer;
use rustls_pki_types::CertificateDer;
use tokio::sync::watch;

use crate::credentials::ProtocolInfo;

pub mod client;
mod tls_stream;

const ALPN_PROTO_STR_H2: &[u8; 2] = b"h2";

/// Represents a X509 certificate chain.
#[derive(Debug, Clone)]
pub struct RootCertificates {
    pem: Vec<u8>,
}

impl RootCertificates {
    /// Parse a PEM encoded X509 Certificate.
    ///
    /// The provided PEM should include at least one PEM encoded certificate.
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Self {
        let pem = pem.as_ref().into();
        Self { pem }
    }

    /// Get a immutable reference to underlying certificate
    fn get_ref(&self) -> &[u8] {
        self.pem.as_slice()
    }
}

/// Represents a private key and X509 certificate chain.
#[derive(Debug, Clone)]
pub struct Identity {
    certs: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    /// Parse a PEM encoded certificate and private key.
    ///
    /// The provided cert must contain at least one PEM encoded certificate.
    pub fn from_pem(cert: impl AsRef<[u8]>, key: impl AsRef<[u8]>) -> Self {
        let cert = cert.as_ref().into();
        let key = key.as_ref().into();
        Self { certs: cert, key }
    }
}

mod provider {
    use tokio::sync::watch::Receiver;

    /// A sealed trait to prevent downstream implementations of `Provider`.
    ///
    /// This trait exposes the internal mechanism (Tokio watch channel) used to
    /// receive updates. It is kept private/restricted to ensure that `Provider`
    /// can only be implemented by types defined within this crate.
    pub trait ProviderInternal<T> {
        /// Returns a clone of the underlying watch receiver.
        ///
        /// This allows the consumer to observe the current value and await
        /// future updates.
        fn get_receiver(self) -> Receiver<T>;
    }
}

/// A source of configuration or state of type `T` that allows for dynamic
/// updates.
///
/// This trait abstracts over the source of the data (e.g., static memory,
/// file system, network) and provides a uniform interface for consumers to
/// access the current value and subscribe to changes.
///
/// # Sealed Trait
///
/// This trait is **sealed**. It cannot be implemented by downstream crates.
/// Users should rely on the provided implementations (e.g.,
/// `StaticIdentityProvider`, `StaticRootsProvider`).
pub trait Provider<T>: provider::ProviderInternal<T> {}

/// A provider that supplies a constant, immutable value.
pub struct StaticProvider<T> {
    inner: T,
}

impl<T> StaticProvider<T> {
    /// Creates a new `StaticProvider` with the given fixed value.
    pub fn new(value: T) -> Self {
        Self { inner: value }
    }
}

impl<T> provider::ProviderInternal<T> for StaticProvider<T> {
    fn get_receiver(self) -> watch::Receiver<T> {
        // We drop the sender (_) immediately.
        // This ensures the receiver sees the initial value but knows
        // no future updates will arrive.
        let (_tx, rx) = watch::channel(self.inner);
        rx
    }
}

impl<T> Provider<T> for StaticProvider<T> {}

pub type StaticRootCertificatesProvider = StaticProvider<RootCertificates>;
pub type StaticIdentityProvider = StaticProvider<Identity>;

static TLS_PROTO_INFO: ProtocolInfo = ProtocolInfo {
    security_protocol: "tls",
};

fn sanitize_crypto_provider(mut crypto_provider: CryptoProvider) -> Result<CryptoProvider, String> {
    crypto_provider.cipher_suites.retain(|suite| match suite {
        rustls::SupportedCipherSuite::Tls13(suite) => true,
        rustls::SupportedCipherSuite::Tls12(suite) => {
            matches!(
                suite.common.suite,
                rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
                    | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
                    | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
                    | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
                    | rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256
                    | rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256
            )
        }
    });

    if crypto_provider.cipher_suites.is_empty() {
        return Err("Crypto provider has no cipher suites matching the security policy (TLS1.3 or TLS1.2+ECDHE)".to_string());
    }

    Ok(crypto_provider)
}

fn parse_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut reader = BufReader::new(pem);
    rustls_pemfile::certs(&mut reader)
        .map(|result| result.map_err(|e| e.to_string()))
        .collect()
}

fn parse_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
    let mut reader = BufReader::new(pem);
    loop {
        match rustls_pemfile::read_one(&mut reader).map_err(|e| e.to_string())? {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Sec1Key(key)) => return Ok(key.into()),
            None => return Err("no private key found".to_string()),
            _ => continue,
        }
    }
}
