use super::io::BoxedIo;
use crate::transport::{
    server::{Connected, TlsStream},
    Certificate, Identity,
};
#[cfg(feature = "tls-roots")]
use rustls_native_certs;
use std::{fmt, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "tls")]
use tokio_rustls::{
    rustls::{ClientConfig, NoClientAuth, ServerConfig, Session},
    webpki::DNSNameRef,
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
    domain: Arc<String>,
}

impl TlsConnector {
    #[cfg(feature = "tls")]
    pub(crate) fn new_with_rustls_cert(
        ca_cert: Option<Certificate>,
        identity: Option<Identity>,
        domain: String,
    ) -> Result<Self, crate::Error> {
        let mut config = ClientConfig::new();
        config.set_protocols(&[Vec::from(ALPN_H2)]);

        if let Some(identity) = identity {
            let (client_cert, client_key) = rustls_keys::load_identity(identity)?;
            config.set_single_client_cert(client_cert, client_key)?;
        }

        #[cfg(feature = "tls-roots")]
        {
            config.root_store = match rustls_native_certs::load_native_certs() {
                Ok(store) | Err((Some(store), _)) => store,
                Err((None, error)) => return Err(error.into()),
            };
        }

        #[cfg(feature = "tls-webpki-roots")]
        {
            config
                .root_store
                .add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
        }

        if let Some(cert) = ca_cert {
            let mut buf = std::io::Cursor::new(&cert.pem[..]);
            config.root_store.add_pem_file(&mut buf).unwrap();
        }

        Ok(Self {
            config: Arc::new(config),
            domain: Arc::new(domain),
        })
    }

    #[cfg(feature = "tls")]
    pub(crate) fn new_with_rustls_raw(
        config: tokio_rustls::rustls::ClientConfig,
        domain: String,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            config: Arc::new(config),
            domain: Arc::new(domain),
        })
    }

    pub(crate) async fn connect<I>(&self, io: I) -> Result<BoxedIo, crate::Error>
    where
        I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let tls_io = {
            let dns = DNSNameRef::try_from_ascii_str(self.domain.as_str())?.to_owned();

            let io = RustlsConnector::from(self.config.clone())
                .connect(dns.as_ref(), io)
                .await?;

            let (_, session) = io.get_ref();

            match session.get_alpn_protocol() {
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
    pub(crate) fn new_with_rustls_identity(
        identity: Identity,
        client_ca_root: Option<Certificate>,
    ) -> Result<Self, crate::Error> {
        let (cert, key) = rustls_keys::load_identity(identity)?;

        let mut config = match client_ca_root {
            None => ServerConfig::new(NoClientAuth::new()),
            Some(cert) => {
                let mut cert = std::io::Cursor::new(&cert.pem[..]);

                let mut client_root_cert_store = tokio_rustls::rustls::RootCertStore::empty();
                if client_root_cert_store.add_pem_file(&mut cert).is_err() {
                    return Err(Box::new(TlsError::CertificateParseError));
                }

                let client_auth =
                    tokio_rustls::rustls::AllowAnyAuthenticatedClient::new(client_root_cert_store);
                ServerConfig::new(client_auth)
            }
        };
        config.set_single_cert(cert, key)?;
        config.set_protocols(&[Vec::from(ALPN_H2)]);

        Ok(Self {
            inner: Arc::new(config),
        })
    }

    #[cfg(feature = "tls")]
    pub(crate) fn new_with_rustls_raw(
        config: tokio_rustls::rustls::ServerConfig,
    ) -> Result<Self, crate::Error> {
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
    use tokio_rustls::rustls::{internal::pemfile, Certificate, PrivateKey};

    use crate::transport::service::tls::TlsError;
    use crate::transport::Identity;

    fn load_rustls_private_key(
        mut cursor: std::io::Cursor<&[u8]>,
    ) -> Result<PrivateKey, crate::Error> {
        // First attempt to load the private key assuming it is PKCS8-encoded
        if let Ok(mut keys) = pemfile::pkcs8_private_keys(&mut cursor) {
            if !keys.is_empty() {
                return Ok(keys.remove(0));
            }
        }

        // If it not, try loading the private key as an RSA key
        cursor.set_position(0);
        if let Ok(mut keys) = pemfile::rsa_private_keys(&mut cursor) {
            if !keys.is_empty() {
                return Ok(keys.remove(0));
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
            match pemfile::certs(&mut cert) {
                Ok(certs) => certs,
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
}
