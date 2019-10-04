use super::io::BoxedIo;
use crate::transport::{Certificate, Identity};
#[cfg(feature = "openssl")]
use openssl1::{
    pkey::PKey,
    ssl::{select_next_proto, AlpnError, SslAcceptor, SslConnector, SslMethod},
    x509::X509,
};
use std::{fmt, sync::Arc};
use tokio::net::TcpStream;
#[cfg(feature = "rustls")]
use tokio_rustls::{
    rustls::{internal::pemfile, ClientConfig, NoClientAuth, PrivateKey, ServerConfig, Session},
    webpki::DNSNameRef,
    TlsAcceptor as RustlsAcceptor, TlsConnector as RustlsConnector,
};
use tracing::trace;

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
    pub(crate) fn new_with_openssl(
        cert: Certificate,
        domain: String,
    ) -> Result<Self, crate::Error> {
        let mut config = SslConnector::builder(SslMethod::tls())?;

        config.set_alpn_protos(ALPN_H2_WIRE)?;

        let ca = X509::from_pem(&cert.pem[..])?;

        config.cert_store_mut().add_cert(ca)?;

        let config = config.build();

        Ok(Self {
            inner: Connector::Openssl(config),
            domain: Arc::new(domain),
        })
    }

    #[cfg(feature = "rustls")]
    pub(crate) fn new_with_rustls(cert: Certificate, domain: String) -> Result<Self, crate::Error> {
        let mut buf = std::io::Cursor::new(&cert.pem[..]);

        let mut config = ClientConfig::new();

        config.root_store.add_pem_file(&mut buf).unwrap();
        config.set_protocols(&[Vec::from(&ALPN_H2[..])]);

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
                    Some(b) if b == b"h2" => trace!("HTTP/2 succesfully negotiated."),
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
    pub(crate) fn new_with_openssl(identity: Identity) -> Result<Self, crate::Error> {
        let key = PKey::private_key_from_pem(&identity.key[..])?;
        let cert = X509::from_pem(&identity.cert.pem[..])?;

        let mut config = SslAcceptor::mozilla_modern(SslMethod::tls())?;

        config.set_private_key(&key)?;
        config.set_certificate(&cert)?;
        config.set_alpn_protos(ALPN_H2_WIRE)?;
        config.set_alpn_select_callback(|_ssl, alpn| {
            select_next_proto(ALPN_H2_WIRE, alpn).ok_or(AlpnError::NOACK)
        });

        Ok(Self {
            inner: Acceptor::Openssl(config.build()),
        })
    }

    #[cfg(feature = "rustls")]
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

    #[cfg(feature = "rustls")]
    pub(crate) fn new_with_rustls(identity: Identity) -> Result<Self, crate::Error> {
        let cert = {
            let mut cert = std::io::Cursor::new(&identity.cert.pem[..]);
            match pemfile::certs(&mut cert) {
                Ok(certs) => certs,
                Err(_) => return Err(Box::new(TlsError::CertificateParseError)),
            }
        };

        let key = {
            let key = std::io::Cursor::new(&identity.key[..]);
            match Self::load_rustls_private_key(key) {
                Ok(key) => key,
                Err(e) => {
                    return Err(e);
                }
            }
        };

        let mut config = ServerConfig::new(NoClientAuth::new());

        config.set_single_cert(cert, key)?;
        config.set_protocols(&[Vec::from(&ALPN_H2[..])]);

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
        }
    }
}

impl std::error::Error for TlsError {}
