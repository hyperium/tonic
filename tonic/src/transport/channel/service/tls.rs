use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::{rustls::ClientConfig, TlsConnector as RustlsConnector};

use super::io::BoxedIo;
use crate::transport::service::tls::{add_certs_from_pem, load_identity, ALPN_H2};
use crate::transport::tls::{Certificate, Identity};

#[derive(Debug)]
enum TlsError {
    H2NotNegotiated,
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::H2NotNegotiated => write!(f, "HTTP/2 was not negotiated."),
        }
    }
}

impl std::error::Error for TlsError {}

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

#[cfg(feature = "channel")]
impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConnector").finish()
    }
}
