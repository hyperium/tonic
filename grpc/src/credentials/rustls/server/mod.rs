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

use std::path::PathBuf;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::server::ClientHello;
use rustls::server::ProducesTickets;
use rustls::server::ResolvesServerCert;
use rustls::sign::CertifiedKey;
use tokio::sync::watch::Receiver;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsStream as RustlsStream;
use webpki::EndEntityCert;

use crate::attributes::Attributes;
use crate::credentials::ProtocolInfo;
use crate::credentials::SecurityLevel;
use crate::credentials::ServerCredentials;
use crate::credentials::rustls::ALPN_PROTO_STR_H2;
use crate::credentials::rustls::IdentityList;
use crate::credentials::rustls::Provider;
use crate::credentials::rustls::RootCertificates;
use crate::credentials::rustls::StaticRootCertificatesProvider;
use crate::credentials::rustls::TLS_PROTO_INFO;
use crate::credentials::rustls::key_log::KeyLogFile;
use crate::credentials::rustls::parse_certs;
use crate::credentials::rustls::parse_key;
use crate::credentials::rustls::sanitize_crypto_provider;
use crate::credentials::rustls::tls_stream::TlsStream;
use crate::credentials::server::HandshakeOutput;
use crate::credentials::server::ServerConnectionSecurityInfo;
use crate::private;
use crate::rt::AsyncIoAdapter;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

#[cfg(test)]
mod test;

#[derive(Debug)]
struct SniResolver {
    keys: Vec<Arc<CertifiedKey>>,
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, client_hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        if self.keys.len() == 1 {
            return Some(self.keys[0].clone());
        }

        if let Some(subject_name) = client_hello
            .server_name()
            .and_then(|sni| rustls_pki_types::ServerName::try_from(sni).ok())
        {
            for key in &self.keys {
                let Some(cert) = key.cert.first() else {
                    continue;
                };
                let Ok(end_entity_cert) = EndEntityCert::try_from(cert) else {
                    continue;
                };

                if end_entity_cert
                    .verify_is_valid_for_subject_name(&subject_name)
                    .is_ok()
                {
                    return Some(key.clone());
                }
            }
        }
        self.keys.first().cloned()
    }
}

#[non_exhaustive]
pub enum TlsClientCertificateRequestType<R = StaticRootCertificatesProvider> {
    /// Server does not request client certificate.
    ///
    /// This is the default behavior.
    ///
    /// The certificate presented by the client is not checked by the server at
    /// all. (A client may present a self-signed or signed certificate or not
    /// present a certificate at all and any of those option would be accepted).
    DontRequest,

    /// Server requests client certificate but does not enforce that the client
    /// presents a certificate.
    ///
    /// If the client presents a certificate, the client authentication is done by
    /// the gRPC framework. For a successful connection the client needs to either
    /// present a certificate that can be verified against the `pem_root_certs`
    /// or not present a certificate at all.
    ///
    /// The client's key certificate pair must be valid for the TLS connection to
    /// be established.
    RequestAndVerify { roots_provider: R },

    /// Server requests client certificate and enforces that the client presents a
    /// certificate.
    ///
    /// The certificate presented by the client is verified by the gRPC framework.
    /// For a successful connection the client needs to present a certificate that
    /// can be verified against the `pem_root_certs`.
    ///
    /// The client's key certificate pair must be valid for the TLS connection to
    /// be established.
    RequireAndVerify { roots_provider: R },
}

enum InnerClientCertificateRequestType {
    DontRequestClientCertificate,
    RequestClientCertificateAndVerify {
        roots_provider: Receiver<RootCertificates>,
    },
    RequestAndRequireClientCertificateAndVerify {
        roots_provider: Receiver<RootCertificates>,
    },
}

impl From<TlsClientCertificateRequestType> for InnerClientCertificateRequestType {
    fn from(value: TlsClientCertificateRequestType) -> Self {
        match value {
            TlsClientCertificateRequestType::DontRequest => {
                InnerClientCertificateRequestType::DontRequestClientCertificate
            }
            TlsClientCertificateRequestType::RequestAndVerify { roots_provider } => {
                InnerClientCertificateRequestType::RequestClientCertificateAndVerify {
                    roots_provider: roots_provider.get_receiver(private::Internal),
                }
            }
            TlsClientCertificateRequestType::RequireAndVerify { roots_provider } => {
                InnerClientCertificateRequestType::RequestAndRequireClientCertificateAndVerify {
                    roots_provider: roots_provider.get_receiver(private::Internal),
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct RustlsServerCredendials {
    acceptor: TlsAcceptor,
}

/// Configuration for server-side TLS settings.
pub struct ServerTlsConfig {
    identities_provider: Receiver<IdentityList>,
    request_type: InnerClientCertificateRequestType,
    key_log_path: Option<PathBuf>,
}

impl ServerTlsConfig {
    pub fn new<I>(identities_provider: I) -> Self
    where
        I: Provider<IdentityList>,
    {
        ServerTlsConfig {
            identities_provider: identities_provider.get_receiver(private::Internal),
            request_type: TlsClientCertificateRequestType::DontRequest.into(),
            key_log_path: None,
        }
    }

    /// Configures the client certificate request policy for the server.
    ///
    /// This determines whether the server requests a client certificate and how
    /// it verifies it.
    pub fn with_request_type(mut self, request_type: TlsClientCertificateRequestType) -> Self {
        self.request_type = request_type.into();
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

impl RustlsServerCredendials {
    pub fn new(config: ServerTlsConfig) -> Result<RustlsServerCredendials, String> {
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
        mut config: ServerTlsConfig,
        provider: CryptoProvider,
    ) -> Result<RustlsServerCredendials, String> {
        let provider = sanitize_crypto_provider(provider)?;
        let id_list = config.identities_provider.borrow_and_update().clone();
        if id_list.is_empty() {
            return Err("need at least one server identity.".to_string());
        }

        let verifier = match config.request_type {
            InnerClientCertificateRequestType::DontRequestClientCertificate => {
                rustls::server::WebPkiClientVerifier::no_client_auth()
            }
            InnerClientCertificateRequestType::RequestClientCertificateAndVerify {
                mut roots_provider,
            } => {
                let roots = roots_provider.borrow_and_update();
                let certs = parse_certs(&roots.pem)?;
                let mut root_store = rustls::RootCertStore::empty();
                for cert in certs {
                    root_store.add(cert).map_err(|e| e.to_string())?;
                }
                rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                    .allow_unauthenticated()
                    .build()
                    .map_err(|e| e.to_string())?
            }
            InnerClientCertificateRequestType::RequestAndRequireClientCertificateAndVerify {
                mut roots_provider,
            } => {
                let roots = roots_provider.borrow_and_update();
                let certs = parse_certs(&roots.pem)?;
                let mut root_store = rustls::RootCertStore::empty();
                for cert in certs {
                    root_store.add(cert).map_err(|e| e.to_string())?;
                }
                rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                    .build()
                    .map_err(|e| e.to_string())?
            }
        };

        let builder = rustls::ServerConfig::builder_with_provider(Arc::new(provider.clone()))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|e| e.to_string())?
            .with_client_cert_verifier(verifier);

        let mut keys = Vec::with_capacity(id_list.len());
        for identity in id_list {
            let certs = parse_certs(&identity.certs)?;
            let key = parse_key(&identity.key)?;
            let signing_key = provider
                .key_provider
                .load_private_key(key)
                .map_err(|e| e.to_string())?;

            keys.push(Arc::new(CertifiedKey::new(certs, signing_key)));
        }

        let resolver = Arc::new(SniResolver { keys });
        let mut server_config = builder.with_cert_resolver(resolver);

        server_config.alpn_protocols = vec![ALPN_PROTO_STR_H2.to_vec()];
        if let Some(path) = config.key_log_path {
            server_config.key_log = Arc::new(KeyLogFile::new(&path));
        }
        // Disable Stateful Resumption (Session IDs).
        server_config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});

        // Disable Stateless Resumption (TLS 1.3 Tickets).
        server_config.send_tls13_tickets = 0;
        // Disable Stateless Resumption (TLS 1.2 Tickets)
        // Install a dummy ticketer that refuses to issue tickets.
        server_config.ticketer = Arc::new(NoTicketer);

        Ok(RustlsServerCredendials {
            acceptor: TlsAcceptor::from(Arc::new(server_config)),
        })
    }
}

// Helper Struct to Disable Tickets.
#[derive(Debug)]
struct NoTicketer;

impl ProducesTickets for NoTicketer {
    fn enabled(&self) -> bool {
        false
    }
    fn lifetime(&self) -> u32 {
        0
    }
    fn encrypt(&self, _plain: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn decrypt(&self, _cipher: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

impl ServerCredentials for RustlsServerCredendials {
    type Output<Input> = TlsStream<Input>;

    async fn accept<Input: GrpcEndpoint>(
        &self,
        source: Input,
        _runtime: GrpcRuntime,
        _token: private::Internal,
    ) -> Result<HandshakeOutput<Self::Output<Input>>, String> {
        let input_io = AsyncIoAdapter::new(source);
        let tls_stream = self
            .acceptor
            .accept(input_io)
            .await
            .map_err(|e| e.to_string())?;

        let (_io, conn) = tls_stream.get_ref();
        if conn.alpn_protocol() != Some(ALPN_PROTO_STR_H2) {
            return Err("Client ignored ALPN requirements".into());
        }

        let auth_info = ServerConnectionSecurityInfo::new(
            "tls",
            SecurityLevel::PrivacyAndIntegrity,
            Attributes::new(),
        );
        let endpoint = TlsStream::new(RustlsStream::Server(tls_stream));
        Ok(HandshakeOutput {
            endpoint,
            security: auth_info,
        })
    }

    fn info(&self) -> &ProtocolInfo {
        &TLS_PROTO_INFO
    }
}
