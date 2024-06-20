#[cfg(feature = "server")]
use std::sync::Arc;
use std::{fmt, io::Cursor};

#[cfg(feature = "server")]
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    RootCertStore,
};
#[cfg(feature = "server")]
use tokio_rustls::{
    rustls::{server::WebPkiClientVerifier, ServerConfig},
    TlsAcceptor as RustlsAcceptor,
};

use crate::transport::Identity;
#[cfg(feature = "server")]
use crate::transport::{
    server::{Connected, TlsStream},
    Certificate,
};

/// h2 alpn in plain format for rustls.
pub(crate) const ALPN_H2: &[u8] = b"h2";

#[derive(Debug)]
pub(crate) enum TlsError {
    #[cfg(feature = "channel")]
    H2NotNegotiated,
    CertificateParseError,
    PrivateKeyParseError,
}

#[cfg(feature = "server")]
#[derive(Clone)]
pub(crate) struct TlsAcceptor {
    inner: Arc<ServerConfig>,
}

#[cfg(feature = "server")]
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

#[cfg(feature = "server")]
impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor").finish()
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "channel")]
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

pub(crate) fn load_identity(
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

pub(crate) fn add_certs_from_pem(
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
