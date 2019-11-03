use super::io::BoxedIo;
use crate::transport::{Certificate, Identity};
#[cfg(feature = "openssl")]
use openssl1::{
    pkey::PKey,
    ssl::{select_next_proto, AlpnError, SslAcceptor, SslConnector, SslMethod, SslVerifyMode},
    x509::{store::X509StoreBuilder, X509},
};
#[cfg(feature = "openssl-roots")]
use openssl_probe;
#[cfg(feature = "rustls-roots")]
use rustls_native_certs;
use std::{fmt, sync::Arc};
use tokio::net::TcpStream;
#[cfg(feature = "rustls")]
use tokio_rustls::{
    rustls::{ClientConfig, NoClientAuth, ServerConfig, Session},
    webpki::DNSNameRef,
    TlsAcceptor as RustlsAcceptor, TlsConnector as RustlsConnector,
};

/// h2 alpn in wire format for openssl.
#[cfg(feature = "openssl")]
const ALPN_H2_WIRE: &[u8] = b"\x02h2";
/// h2 alpn in plain format for rustls.
#[cfg(feature = "rustls")]
const ALPN_H2: &str = "h2";

#[derive(Debug, Clone)]
pub(crate) struct Cert {
    pub(crate) ca: Vec<u8>,
    pub(crate) key: Option<Vec<u8>>,
    pub(crate) domain: String,
}

#[derive(Debug)]
enum TlsError {
    #[allow(dead_code)]
    H2NotNegotiated,
    #[cfg(feature = "rustls")]
    CertificateParseError,
    #[cfg(feature = "rustls")]
    PrivateKeyParseError,
    #[cfg(feature = "openssl-roots")]
    TrustAnchorsConfigurationError(openssl1::error::ErrorStack),
}

#[derive(Clone)]
pub(crate) struct TlsConnector {
    inner: Connector,
    domain: Arc<String>,
}

#[derive(Clone)]
enum Connector {
    #[cfg(feature = "openssl")]
    Openssl(SslConnector),
    #[cfg(feature = "rustls")]
    Rustls(Arc<ClientConfig>),
}

impl TlsConnector {
    #[cfg(feature = "openssl")]
    pub(crate) fn new_with_openssl_cert(
        cert: Option<Certificate>,
        identity: Option<Identity>,
        domain: String,
    ) -> Result<Self, crate::Error> {
        let mut config = SslConnector::builder(SslMethod::tls())?;
        config.set_alpn_protos(ALPN_H2_WIRE)?;

        #[cfg(feature = "openssl-roots")]
        {
            openssl_probe::init_ssl_cert_env_vars();
            match config.cert_store_mut().set_default_paths() {
                Ok(()) => (),
                Err(e) => return Err(Box::new(TlsError::TrustAnchorsConfigurationError(e))),
            };
        }

        if let Some(cert) = cert {
            let ca = X509::from_pem(&cert.pem[..])?;
            config.cert_store_mut().add_cert(ca)?;
        }

        if let Some(identity) = identity {
            let key = PKey::private_key_from_pem(&identity.key[..])?;
            let cert = X509::from_pem(&identity.cert.pem[..])?;
            config.set_certificate(&cert)?;
            config.set_private_key(&key)?;
        }

        Ok(Self {
            inner: Connector::Openssl(config.build()),
            domain: Arc::new(domain),
        })
    }

    #[cfg(feature = "openssl")]
    pub(crate) fn new_with_openssl_raw(
        ssl_connector: openssl1::ssl::SslConnector,
        domain: String,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            inner: Connector::Openssl(ssl_connector),
            domain: Arc::new(domain),
        })
    }

    #[cfg(feature = "rustls")]
    pub(crate) fn new_with_rustls_cert(
        ca_cert: Option<Certificate>,
        identity: Option<Identity>,
        domain: String,
    ) -> Result<Self, crate::Error> {
        let mut config = ClientConfig::new();
        config.set_protocols(&[Vec::from(&ALPN_H2[..])]);

        if let Some(identity) = identity {
            let (client_cert, client_key) = rustls_keys::load_identity(identity)?;
            config.set_single_client_cert(client_cert, client_key);
        }

        #[cfg(feature = "rustls-roots")]
        {
            config.root_store = rustls_native_certs::load_native_certs()?;
        }

        if let Some(cert) = ca_cert {
            let mut buf = std::io::Cursor::new(&cert.pem[..]);
            config.root_store.add_pem_file(&mut buf).unwrap();
        }

        Ok(Self {
            inner: Connector::Rustls(Arc::new(config)),
            domain: Arc::new(domain),
        })
    }

    #[cfg(feature = "rustls")]
    pub(crate) fn new_with_rustls_raw(
        config: tokio_rustls::rustls::ClientConfig,
        domain: String,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            inner: Connector::Rustls(Arc::new(config)),
            domain: Arc::new(domain),
        })
    }

    pub(crate) async fn connect(&self, io: TcpStream) -> Result<BoxedIo, crate::Error> {
        let tls_io = match &self.inner {
            #[cfg(feature = "openssl")]
            Connector::Openssl(connector) => {
                let config = connector.configure()?;
                let tls = tokio_openssl::connect(config, &self.domain, io).await?;

                match tls.ssl().selected_alpn_protocol() {
                    Some(b) if b == b"h2" => tracing::trace!("HTTP/2 succesfully negotiated."),
                    _ => return Err(TlsError::H2NotNegotiated.into()),
                };

                BoxedIo::new(tls)
            }
            #[cfg(feature = "rustls")]
            Connector::Rustls(config) => {
                let dns = DNSNameRef::try_from_ascii_str(self.domain.as_str())
                    .unwrap()
                    .to_owned();

                let io = RustlsConnector::from(config.clone())
                    .connect(dns.as_ref(), io)
                    .await?;

                let (_, session) = io.get_ref();

                match session.get_alpn_protocol() {
                    Some(b) if b == b"h2" => (),
                    _ => return Err(TlsError::H2NotNegotiated.into()),
                };

                BoxedIo::new(io)
            }

            #[allow(unreachable_patterns)]
            _ => unreachable!("Reached a tls config point with neither feature enabled!"),
        };

        Ok(tls_io)
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConnector")
            .field(
                "inner",
                match &self.inner {
                    #[cfg(feature = "openssl")]
                    Connector::Openssl(_) => &"Openssl",
                    #[cfg(feature = "rustls")]
                    Connector::Rustls(_) => &"Rustls",

                    #[allow(unreachable_patterns)]
                    _ => &"None",
                },
            )
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct TlsAcceptor {
    inner: Acceptor,
}

#[derive(Clone)]
enum Acceptor {
    #[cfg(feature = "openssl")]
    Openssl(SslAcceptor),
    #[cfg(feature = "rustls")]
    Rustls(Arc<ServerConfig>),
}

impl TlsAcceptor {
    #[cfg(feature = "openssl")]
    pub(crate) fn new_with_openssl_identity(
        identity: Identity,
        client_ca_root: Option<Certificate>,
    ) -> Result<Self, crate::Error> {
        let key = PKey::private_key_from_pem(&identity.key[..])?;
        let cert = X509::from_pem(&identity.cert.pem[..])?;

        let mut config = SslAcceptor::mozilla_modern(SslMethod::tls())?;

        config.set_private_key(&key)?;
        config.set_certificate(&cert)?;
        config.set_alpn_protos(ALPN_H2_WIRE)?;
        config.set_alpn_select_callback(|_ssl, alpn| {
            select_next_proto(ALPN_H2_WIRE, alpn).ok_or(AlpnError::NOACK)
        });

        if let Some(cert) = client_ca_root {
            let ca_cert = X509::from_pem(&cert.pem[..])?;
            let mut store = X509StoreBuilder::new()?;
            store.add_cert(ca_cert.clone())?;

            config.add_client_ca(&ca_cert)?;
            config.set_verify_cert_store(store.build())?;
            config.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
        }

        Ok(Self {
            inner: Acceptor::Openssl(config.build()),
        })
    }

    #[cfg(feature = "openssl")]
    pub(crate) fn new_with_openssl_raw(
        acceptor: openssl1::ssl::SslAcceptor,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            inner: Acceptor::Openssl(acceptor),
        })
    }

    #[cfg(feature = "rustls")]
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
                match client_root_cert_store.add_pem_file(&mut cert) {
                    Err(_) => return Err(Box::new(TlsError::CertificateParseError)),
                    _ => (),
                };

                let client_auth =
                    tokio_rustls::rustls::AllowAnyAuthenticatedClient::new(client_root_cert_store);
                ServerConfig::new(client_auth)
            }
        };
        config.set_single_cert(cert, key)?;
        config.set_protocols(&[Vec::from(&ALPN_H2[..])]);

        Ok(Self {
            inner: Acceptor::Rustls(Arc::new(config)),
        })
    }

    #[cfg(feature = "rustls")]
    pub(crate) fn new_with_rustls_raw(
        config: tokio_rustls::rustls::ServerConfig,
    ) -> Result<Self, crate::Error> {
        Ok(Self {
            inner: Acceptor::Rustls(Arc::new(config)),
        })
    }

    pub(crate) async fn connect(&self, io: TcpStream) -> Result<BoxedIo, crate::Error> {
        let io = match &self.inner {
            #[cfg(feature = "openssl")]
            Acceptor::Openssl(acceptor) => {
                let tls = tokio_openssl::accept(&acceptor, io).await?;
                BoxedIo::new(tls)
            }

            #[cfg(feature = "rustls")]
            Acceptor::Rustls(config) => {
                let acceptor = RustlsAcceptor::from(config.clone());
                let tls = acceptor.accept(io).await?;
                BoxedIo::new(tls)
            }

            #[allow(unreachable_patterns)]
            _ => unreachable!("Reached a tls config point with neither feature enabled!"),
        };

        Ok(io)
    }
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor")
            .field(
                "inner",
                match &self.inner {
                    #[cfg(feature = "openssl")]
                    Acceptor::Openssl(_) => &"Openssl",
                    #[cfg(feature = "rustls")]
                    Acceptor::Rustls(_) => &"Rustls",
                    #[allow(unreachable_patterns)]
                    _ => &"None",
                },
            )
            .finish()
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::H2NotNegotiated => write!(f, "HTTP/2 was not negotiated."),
            #[cfg(feature = "rustls")]
            TlsError::CertificateParseError => write!(f, "Error parsing TLS certificate."),
            #[cfg(feature = "rustls")]
            TlsError::PrivateKeyParseError => write!(
                f,
                "Error parsing TLS private key - no RSA or PKCS8-encoded keys found."
            ),
            #[cfg(feature = "openssl-roots")]
            TlsError::TrustAnchorsConfigurationError(stack) => {
                f.write_fmt(format_args!("Error adding trust anchors - {}", stack))
            }
        }
    }
}

impl std::error::Error for TlsError {}

#[cfg(feature = "rustls")]
mod rustls_keys {
    use tokio_rustls::rustls::{internal::pemfile, Certificate, PrivateKey};

    use crate::transport::service::tls::TlsError;
    use crate::transport::Identity;

    fn load_rustls_private_key(
        mut cursor: std::io::Cursor<&[u8]>,
    ) -> Result<PrivateKey, crate::Error> {
        // First attempt to load the private key assuming it is PKCS8-encoded
        if let Ok(mut keys) = pemfile::pkcs8_private_keys(&mut cursor) {
            if keys.len() > 0 {
                return Ok(keys.remove(0));
            }
        }

        // If it not, try loading the private key as an RSA key
        cursor.set_position(0);
        if let Ok(mut keys) = pemfile::rsa_private_keys(&mut cursor) {
            if keys.len() > 0 {
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
