use std::{
    io::Cursor,
    {fmt, sync::Arc},
};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{
    rustls::{server::WebPkiClientVerifier, ClientConfig, RootCertStore, ServerConfig},
    TlsAcceptor as RustlsAcceptor, TlsConnector as RustlsConnector,
};

use super::io::BoxedIo;
use crate::transport::{
    server::{Connected, TlsStream},
    Certificate, Identity,
};

/// h2 alpn in plain format for rustls.
const ALPN_H2: &[u8] = b"h2";

#[derive(Debug)]
enum TlsError {
    H2NotNegotiated,
    CertificateParseError,
    PrivateKeyParseError,
}

#[derive(Clone)]
pub(crate) struct TlsConnector {
    config: Arc<ClientConfig>,
    domain: Arc<ServerName<'static>>,
}

impl TlsConnector {
    pub(crate) fn new(
        ca_cert: Option<Certificate>,
        identity: Option<Identity>,
        domain: &str,
    ) -> Result<Self, crate::Error> {
        let builder = ClientConfig::builder();
        let mut roots = RootCertStore::empty();

        #[cfg(feature = "tls-roots")]
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs()?);

        #[cfg(feature = "tls-webpki-roots")]
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        if let Some(cert) = ca_cert {
            add_certs_from_pem(&mut Cursor::new(cert), &mut roots)?;
        }

        let builder = builder.with_root_certificates(roots);
        let mut config = match identity {
            Some(identity) => {
                let (client_cert, client_key) = load_identity(identity)?;
                builder.with_client_auth_cert(client_cert, client_key)?
            }
            None => builder.with_no_client_auth(),
        };

        config.alpn_protocols.push(ALPN_H2.into());
        Ok(Self {
            config: Arc::new(config),
            domain: Arc::new(ServerName::try_from(domain)?.to_owned()),
        })
    }

    pub(crate) async fn connect<I>(&self, io: I) -> Result<BoxedIo, crate::Error>
    where
        I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let io = RustlsConnector::from(self.config.clone())
            .connect(self.domain.as_ref().to_owned(), io)
            .await?;

        let (_, session) = io.get_ref();
        if session.alpn_protocol() != Some(ALPN_H2) {
            return Err(TlsError::H2NotNegotiated)?;
        }

        Ok(BoxedIo::new(io))
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConnector").finish()
    }
}

#[derive(Clone)]
pub(crate) struct TlsAcceptor {
    inner: Arc<ServerConfig>,
}

impl TlsAcceptor {
    pub(crate) fn new(
        identity: Identity,
        client_ca_root: Option<Certificate>,
        client_auth_optional: bool,
    ) -> Result<Self, crate::Error> {
        let builder = ServerConfig::builder();

        let builder = match client_ca_root {
            None => builder.with_no_client_auth(),
            Some(cert) => {
                let mut roots = RootCertStore::empty();
                add_certs_from_pem(&mut Cursor::new(cert), &mut roots)?;
                let verifier = if client_auth_optional {
                    WebPkiClientVerifier::builder(roots.into()).allow_unauthenticated()
                } else {
                    WebPkiClientVerifier::builder(roots.into())
                }
                .build()?;
                builder.with_client_cert_verifier(verifier)
            }
        };

        let (cert, key) = load_identity(identity)?;
        let mut config = builder.with_single_cert(cert, key)?;

        config.alpn_protocols.push(ALPN_H2.into());
        Ok(Self {
            inner: Arc::new(config),
        })
    }

    pub(crate) async fn accept<IO>(&self, io: IO) -> Result<TlsStream<IO>, crate::Error>
    where
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    {
        let acceptor = RustlsAcceptor::from(self.inner.clone());
        acceptor.accept(io).await.map_err(Into::into)
    }
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor").finish()
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::H2NotNegotiated => write!(f, "HTTP/2 was not negotiated."),
            TlsError::CertificateParseError => write!(f, "Error parsing TLS certificate."),
            TlsError::PrivateKeyParseError => write!(
                f,
                "Error parsing TLS private key - no RSA or PKCS8-encoded keys found."
            ),
        }
    }
}

impl std::error::Error for TlsError {}

fn load_identity(
    identity: Identity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    let cert = rustls_pemfile::certs(&mut Cursor::new(identity.cert))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsError::CertificateParseError)?;

    let Ok(Some(key)) = rustls_pemfile::private_key(&mut Cursor::new(identity.key)) else {
        return Err(TlsError::PrivateKeyParseError);
    };

    Ok((cert, key))
}

fn add_certs_from_pem(
    mut certs: &mut dyn std::io::BufRead,
    roots: &mut RootCertStore,
) -> Result<(), crate::Error> {
    for cert in rustls_pemfile::certs(&mut certs).collect::<Result<Vec<_>, _>>()? {
        roots
            .add(cert)
            .map_err(|_| TlsError::CertificateParseError)?;
    }

    Ok(())
}
