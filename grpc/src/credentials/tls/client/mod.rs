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

use std::{path::PathBuf, sync::Arc};

use rustls::crypto::CryptoProvider;
use rustls_pki_types::{CertificateDer, ServerName};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::sync::watch::Receiver;
use tokio_rustls::{TlsConnector, TlsStream as RustlsStream};

use crate::attributes::Attributes;
use crate::credentials::client::{
    self, ClientConnectionSecurityContext, ClientConnectionSecurityInfo, ClientHandshakeInfo,
    HandshakeOutput,
};
use crate::credentials::common::{Authority, SecurityLevel};
use crate::credentials::tls::key_log::KeyLogFile;
use crate::credentials::tls::tls_stream::TlsStream;
use crate::credentials::tls::{
    parse_certs, parse_key, sanitize_crypto_provider, Identity, Provider, RootCertificates,
    TLS_PROTO_INFO,
};
use crate::credentials::{ChannelCredentials, ProtocolInfo};
use crate::rt::{GrpcEndpoint, GrpcRuntime};

#[cfg(test)]
mod test;

/// Configuration for client-side TLS settings.
pub struct ClientTlsConfig {
    pem_roots_provider: Option<Receiver<RootCertificates>>,
    identity_provider: Option<Receiver<Identity>>,
    key_log_path: Option<PathBuf>,
}

impl ClientTlsConfig {
    pub fn new() -> Self {
        ClientTlsConfig {
            pem_roots_provider: None,
            identity_provider: None,
            key_log_path: None,
        }
    }

    /// Configures the set of PEM-encoded root certificates (CA) to trust.
    ///
    /// These certificates are used to validate the server's certificate chain.
    /// If this is not called, the client generally defaults to using the
    /// system's native certificate store.
    pub fn with_root_certificates_provider<R>(mut self, provider: R) -> Self
    where
        R: Provider<RootCertificates>,
    {
        self.pem_roots_provider = Some(provider.get_receiver());
        self
    }

    /// Configures the client's identity for Mutual TLS (mTLS).
    ///
    /// This provides the client's certificate chain and private key.
    /// If this is not called, the client will not present a certificate
    /// to the server (standard one-way TLS).
    pub fn with_identity_provider<I>(mut self, provider: I) -> Self
    where
        I: Provider<Identity>,
    {
        self.identity_provider = Some(provider.get_receiver());
        self
    }

    /// Sets the path where TLS session keys will be logged.
    ///
    /// # Security
    ///
    /// This should be used **only for debugging purposes**. It should never be
    /// used in a production environment due to security concerns.
    pub fn with_key_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.key_log_path = Some(path.into());
        self
    }
}

#[derive(Clone)]
pub struct RustlsClientTlsCredendials {
    connector: TlsConnector,
}

impl RustlsClientTlsCredendials {
    /// Constructs a new `ClientTlsCredendials` instance from the provided
    /// configuration.
    pub fn new(config: ClientTlsConfig) -> Result<RustlsClientTlsCredendials, String> {
        let provider = if let Some(p) = CryptoProvider::get_default() {
            p.as_ref().clone()
        } else {
            return Err(
            "No crypto provider installed. Enable `tls-aws-lc` feature or install one manually."
                .to_string(),
        );
        };

        Self::new_impl(config, provider)
    }

    fn new_impl(
        mut config: ClientTlsConfig,
        provider: CryptoProvider,
    ) -> Result<RustlsClientTlsCredendials, String> {
        let provider = sanitize_crypto_provider(provider)?;
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|e| e.to_string())?;

        let builder = if let Some(mut roots_provider) = config.pem_roots_provider.take() {
            let mut root_store = rustls::RootCertStore::empty();
            let ca_pem = roots_provider.borrow_and_update();
            let certs = parse_certs(ca_pem.as_ref())?;
            for cert in certs {
                root_store.add(cert).map_err(|e| e.to_string())?;
            }
            builder.with_root_certificates(root_store)
        } else {
            // Use system root certificates.
            builder
                .with_platform_verifier()
                .map_err(|e| e.to_string())?
        };

        let mut client_config = if let Some(mut identity_provider) = config.identity_provider.take()
        {
            let identity = identity_provider.borrow_and_update();
            let certs = parse_certs(&identity.cert)?;
            let key = parse_key(&identity.key)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| e.to_string())?
        } else {
            builder.with_no_client_auth()
        };

        client_config.alpn_protocols = vec![b"h2".to_vec()];
        client_config.resumption = rustls::client::Resumption::disabled();
        if let Some(path) = config.key_log_path {
            client_config.key_log = Arc::new(KeyLogFile::new(&path))
        }

        Ok(RustlsClientTlsCredendials {
            connector: TlsConnector::from(Arc::new(client_config)),
        })
    }

    // Test-only constructor that enables injecting a custom crypto provider.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        config: ClientTlsConfig,
        provider: CryptoProvider,
    ) -> Result<RustlsClientTlsCredendials, String> {
        Self::new_impl(config, provider)
    }
}

pub struct ClientTlsSecContext {
    verified_peer_cert: Option<CertificateDer<'static>>,
}

impl ClientConnectionSecurityContext for ClientTlsSecContext {
    fn validate_authority(&self, authority: &Authority) -> bool {
        let server_name = match ServerName::try_from(authority.host()) {
            Ok(n) => n,
            Err(_) => return false,
        };

        let cert_der = match &self.verified_peer_cert {
            Some(c) => c,
            None => return false,
        };

        let cert = match webpki::EndEntityCert::try_from(cert_der) {
            Ok(c) => c,
            Err(_) => return false,
        };

        cert.verify_is_valid_for_subject_name(&server_name).is_ok()
    }
}

impl client::ChannelCredsInternal for RustlsClientTlsCredendials {
    type ContextType = ClientTlsSecContext;
    type Output<I> = TlsStream<I>;
    async fn connect<Input: GrpcEndpoint>(
        &self,
        authority: &Authority,
        source: Input,
        _info: ClientHandshakeInfo,
        _rt: GrpcRuntime,
    ) -> Result<HandshakeOutput<TlsStream<Input>, ClientTlsSecContext>, String> {
        let server_name = ServerName::try_from(authority.host())
            .map_err(|e| format!("invalid authority: {}", e))?
            .to_owned();

        let tls_stream = self
            .connector
            .connect(server_name, source)
            .await
            .map_err(|e| e.to_string())?;

        let (_, connection) = tls_stream.get_ref();
        if let Some(negotiated) = connection.alpn_protocol() {
            if negotiated != b"h2" {
                return Err("Server negotiated unexpected ALPN protocol".into());
            }
        } else {
            // Strict Enforcement: Fail if server didn't select ALPN
            return Err("Server did not negotiate ALPN (h2 required)".into());
        }
        let peer_cert = connection
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|c| c.clone().into_owned());

        let cs_info = ClientConnectionSecurityInfo::new(
            "tls",
            SecurityLevel::PrivacyAndIntegrity,
            ClientTlsSecContext {
                verified_peer_cert: peer_cert,
            },
            Attributes {},
        );
        let ep = TlsStream {
            inner: RustlsStream::Client(tls_stream),
        };
        Ok(HandshakeOutput {
            endpoint: ep,
            security: cs_info,
        })
    }
}

impl ChannelCredentials for RustlsClientTlsCredendials {
    fn info(&self) -> &ProtocolInfo {
        &TLS_PROTO_INFO
    }
}
