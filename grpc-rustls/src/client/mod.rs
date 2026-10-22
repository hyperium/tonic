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

//! Client-side gRPC [`rustls`] [`ChannelCredentials`] implementation.

use std::path::PathBuf;
use std::sync::Arc;

use grpc::__unstable::Internal;
use grpc::__unstable::credentials::client::ClientHandshakeInfo;
use grpc::__unstable::credentials::client::HandshakeOutput;
use grpc::__unstable::credentials::client::ValidateAuthority;
use grpc::__unstable::credentials::common::Authority;
use grpc::__unstable::rt::BoxEndpoint;
use grpc::__unstable::rt::EndpointIoStream;
use grpc::__unstable::rt::GrpcRuntime;
use grpc::async_trait;
use grpc::credentials::ChannelCredentials;
use grpc::credentials::ProtocolInfo;
use grpc::credentials::SecurityInfo;
use grpc::credentials::SecurityLevel;
use grpc::credentials::call::CallCredentials;
use rustls::crypto::CryptoProvider;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::sync::watch::Receiver;
use tokio_rustls::TlsConnector;
use tokio_rustls::TlsStream as RustlsStream;

use crate::ALPN_PROTO_STR_H2;
use crate::Identity;
use crate::Provider;
use crate::RootCertificates;
use crate::TLS_PROTO_INFO;
use crate::key_log::KeyLogFile;
use crate::parse_certs;
use crate::parse_key;
use crate::sanitize_crypto_provider;
use crate::tls_stream::TlsStream;

#[cfg(test)]
mod test;

/// Configuration for client-side TLS settings.
pub struct ClientTlsConfig {
    pem_roots_provider: Option<Receiver<RootCertificates>>,
    identity_provider: Option<Receiver<Identity>>,
    key_log_path: Option<PathBuf>,
}

impl ClientTlsConfig {
    /// Creates a new, unconfigured rustls credentials configuration instance.
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
        self.pem_roots_provider = Some(provider.get_receiver(Internal));
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
        self.identity_provider = Some(provider.get_receiver(Internal));
        self
    }

    /// Sets the path where TLS session keys will be logged.
    ///
    /// # Security
    ///
    /// This should be used **only for debugging purposes**. It should never be
    /// used in a production environment due to security concerns.
    pub fn insecure_with_key_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.key_log_path = Some(path.into());
        self
    }
}

impl Default for ClientTlsConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// gRPC TLS [`ChannelCredentials`] based on [`rustls`].
#[derive(Clone)]
pub struct RustlsChannelCredentials {
    connector: TlsConnector,
}

impl RustlsChannelCredentials {
    /// Constructs a new [RustlsChannelCredentials] instance from the provided
    /// configuration.
    pub fn new(config: ClientTlsConfig) -> Result<RustlsChannelCredentials, String> {
        let provider = if let Some(p) = CryptoProvider::get_default() {
            p.as_ref().clone()
        } else {
            return Err(
                "No crypto provider installed. Enable `tls-aws-lc` feature in rustls or install one manually."
                .to_string()
            );
        };

        Self::new_impl(config, provider)
    }

    fn new_impl(
        mut config: ClientTlsConfig,
        provider: CryptoProvider,
    ) -> Result<RustlsChannelCredentials, String> {
        let provider = sanitize_crypto_provider(provider)?;
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|e| e.to_string())?;

        let builder = if let Some(mut roots_provider) = config.pem_roots_provider.take() {
            let mut root_store = rustls::RootCertStore::empty();
            let ca_pem = roots_provider.borrow_and_update();
            let certs = parse_certs(ca_pem.get_ref())?;
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
            let certs = parse_certs(&identity.certs)?;
            let key = parse_key(&identity.key)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| e.to_string())?
        } else {
            builder.with_no_client_auth()
        };

        client_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
        client_config.resumption = rustls::client::Resumption::disabled();
        if let Some(path) = config.key_log_path {
            client_config.key_log = Arc::new(KeyLogFile::new(&path));
        }

        Ok(RustlsChannelCredentials {
            connector: TlsConnector::from(Arc::new(client_config)),
        })
    }

    pub(crate) async fn connect_tls(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
    ) -> Result<
        (
            TlsStream<BoxEndpoint>,
            SecurityInfo,
            Box<dyn ValidateAuthority>,
        ),
        String,
    > {
        let server_name = ServerName::try_from(authority.host())
            .map_err(|e| format!("invalid authority: {}", e))?
            .to_owned();
        let input_io = EndpointIoStream::new(source);

        let tls_stream = self
            .connector
            .connect(server_name, input_io)
            .await
            .map_err(|e| e.to_string())?;

        let (_io, connection) = tls_stream.get_ref();
        if let Some(negotiated) = connection.alpn_protocol() {
            if negotiated != ALPN_PROTO_STR_H2 {
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

        let cs_info =
            SecurityInfo::new("tls").with_security_level(SecurityLevel::PrivacyAndIntegrity);
        let ep = TlsStream::new(RustlsStream::Client(tls_stream));
        Ok((
            ep,
            cs_info,
            Box::new(ClientTlsAuthorityValidator {
                verified_peer_cert: peer_cert,
            }),
        ))
    }
}

/// Authority validator for [`rustls`]-based gRPC [`ChannelCredentials`].
#[derive(Debug)]
pub struct ClientTlsAuthorityValidator {
    verified_peer_cert: Option<CertificateDer<'static>>,
}

impl ValidateAuthority for ClientTlsAuthorityValidator {
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

#[async_trait]
impl ChannelCredentials for RustlsChannelCredentials {
    async fn connect(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
        _info: &ClientHandshakeInfo,
        _rt: &GrpcRuntime,
        _token: Internal,
    ) -> Result<HandshakeOutput, String> {
        let (ep, security_info, authority_validator) = self.connect_tls(authority, source).await?;
        Ok(HandshakeOutput {
            endpoint: Box::new(ep),
            security_info,
            authority_validator,
        })
    }

    fn info(&self) -> &ProtocolInfo {
        &TLS_PROTO_INFO
    }

    fn get_call_credentials(&self, _: Internal) -> Option<&Arc<dyn CallCredentials>> {
        None
    }
}
