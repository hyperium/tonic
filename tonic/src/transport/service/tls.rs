use super::io::BoxedIo;
use crate::transport::{
    server::{Connected, TlsStream},
    Certificate, Identity,
};
#[cfg(feature = "tls-roots")]
use rustls_native_certs;
#[cfg(feature = "tls")]
use std::convert::TryInto;
use std::{fmt, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "tls")]
use tokio_rustls::{
    rustls::{ClientConfig, RootCertStore, ServerConfig, ServerName},
    TlsAcceptor as RustlsAcceptor, TlsConnector as RustlsConnector,
};

/// h2 alpn in plain format for rustls.
#[cfg(feature = "tls")]
const ALPN_H2: &str = "h2";

#[derive(Debug)]
enum TlsError {
    #[allow(dead_code)]
    H2NotNegotiated,
    #[cfg(feature = "tls")]
    CertificateParseError,
    #[cfg(feature = "tls")]
    PrivateKeyParseError,
}

#[derive(Clone)]
pub(crate) struct TlsConnector {
    config: Arc<ClientConfig>,
    domain: Arc<ServerName>,
}

impl TlsConnector {
    #[cfg(feature = "tls")]
    pub(crate) fn new(
        ca_cert: Option<Certificate>,
        identity: Option<Identity>,
        domain: String,
    ) -> Result<Self, crate::Error> {
        let builder = ClientConfig::builder().with_safe_defaults();
        let mut roots = RootCertStore::empty();

        #[cfg(feature = "tls-roots")]
        {
            match rustls_native_certs::load_native_certs() {
                Ok(certs) => roots.add_parsable_certificates(
                    &certs.into_iter().map(|cert| cert.0).collect::<Vec<_>>(),
                ),
                Err(error) => return Err(error.into()),
            };
        }

        #[cfg(feature = "tls-webpki-roots")]
        {
            use tokio_rustls::rustls::OwnedTrustAnchor;

            roots.add_server_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
                OwnedTrustAnchor::from_subject_spki_name_constraints(
                    ta.subject,
                    ta.spki,
                    ta.name_constraints,
                )
            }));
        }

        if let Some(cert) = ca_cert {
            rustls_keys::add_certs_from_pem(std::io::Cursor::new(&cert.pem[..]), &mut roots)?;
        }

        let builder = builder.with_root_certificates(roots);
        let mut config = match identity {
            Some(identity) => {
                let (client_cert, client_key) = rustls_keys::load_identity(identity)?;
                builder.with_single_cert(client_cert, client_key)?
            }
            None => builder.with_no_client_auth(),
        };

        config.alpn_protocols.push(ALPN_H2.as_bytes().to_vec());
        Ok(Self {
            config: Arc::new(config),
            domain: Arc::new(domain.as_str().try_into()?),
        })
    }

    pub(crate) async fn connect<I>(&self, io: I) -> Result<BoxedIo, crate::Error>
    where
        I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let tls_io = {
            let io = RustlsConnector::from(self.config.clone())
                .connect(self.domain.as_ref().to_owned(), io)
                .await?;

            let (_, session) = io.get_ref();

            match session.alpn_protocol() {
                Some(b) if b == b"h2" => (),
                _ => return Err(TlsError::H2NotNegotiated.into()),
            };

            BoxedIo::new(io)
        };

        Ok(tls_io)
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
    #[cfg(feature = "tls")]
    pub(crate) fn new(
        identity: Identity,
        client_ca_root: Option<Certificate>,
    ) -> Result<Self, crate::Error> {
        let builder = ServerConfig::builder().with_safe_defaults();

        let builder = match client_ca_root {
            None => builder.with_no_client_auth(),
            Some(cert) => {
                use tokio_rustls::rustls::server::AllowAnyAuthenticatedClient;
                let mut roots = RootCertStore::empty();
                rustls_keys::add_certs_from_pem(std::io::Cursor::new(&cert.pem[..]), &mut roots)?;
                builder.with_client_cert_verifier(AllowAnyAuthenticatedClient::new(roots))
            }
        };

        let (cert, key) = rustls_keys::load_identity(identity)?;
        let mut config = builder.with_single_cert(cert, key)?;

        config.alpn_protocols.push(ALPN_H2.as_bytes().to_vec());
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

#[cfg(feature = "tls")]
mod rustls_keys {
    use std::io::Cursor;

    use tokio_rustls::rustls::{Certificate, PrivateKey, RootCertStore};

    use crate::transport::service::tls::TlsError;
    use crate::transport::Identity;

    fn load_rustls_private_key(
        mut cursor: std::io::Cursor<&[u8]>,
    ) -> Result<PrivateKey, crate::Error> {
        // First attempt to load the private key assuming it is PKCS8-encoded
        if let Ok(mut keys) = rustls_pemfile::pkcs8_private_keys(&mut cursor) {
            if let Some(key) = keys.pop() {
                return Ok(PrivateKey(key));
            }
        }

        // If it not, try loading the private key as an RSA key
        cursor.set_position(0);
        if let Ok(mut keys) = rustls_pemfile::rsa_private_keys(&mut cursor) {
            if let Some(key) = keys.pop() {
                return Ok(PrivateKey(key));
            }
        }

        // Otherwise we have a Private Key parsing problem
        Err(Box::new(TlsError::PrivateKeyParseError))
    }

    pub(crate) fn load_identity(
        identity: Identity,
    ) -> Result<(Vec<Certificate>, PrivateKey), crate::Error> {
        let cert = {
            let mut cert = std::io::Cursor::new(&identity.cert.pem[..]);
            match rustls_pemfile::certs(&mut cert) {
                Ok(certs) => certs.into_iter().map(Certificate).collect(),
                Err(_) => return Err(Box::new(TlsError::CertificateParseError)),
            }
        };

        let key = {
            let key = std::io::Cursor::new(&identity.key[..]);
            match load_rustls_private_key(key) {
                Ok(key) => key,
                Err(e) => {
                    return Err(e);
                }
            }
        };

        Ok((cert, key))
    }

    pub(crate) fn add_certs_from_pem(
        mut certs: Cursor<&[u8]>,
        roots: &mut RootCertStore,
    ) -> Result<(), crate::Error> {
        let (_, ignored) = roots.add_parsable_certificates(&rustls_pemfile::certs(&mut certs)?);
        match ignored == 0 {
            true => Ok(()),
            false => Err(Box::new(TlsError::CertificateParseError)),
        }
    }
}
