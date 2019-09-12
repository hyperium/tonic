// TODO: bring back rustls
// #[cfg(feature = "native-tls")]
// #[cfg(not(feature = "rustls"))]
// #[path = "rustls.rs"]
// mod imp;

#[cfg(feature = "native-tls")]
#[cfg(not(feature = "rustls"))]
#[path = "openssl.rs"]
mod imp;

use std::fmt;
use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub(crate) struct Cert {
    pub(crate) ca: Vec<u8>,
    pub(crate) key: Option<Vec<u8>>,
    pub(crate) domain: String,
}

#[derive(Clone)]
pub(crate) struct TlsConnector {
    inner: imp::TlsConnector,
}

impl TlsConnector {
    pub(crate) fn new(cert: Cert) -> Result<Self, crate::Error> {
        let inner = imp::TlsConnector::new(cert)?;
        Ok(Self { inner })
    }

    pub(crate) async fn connect(&self, io: TcpStream) -> Result<imp::TlsStream, crate::Error> {
        self.inner.connect(io).await
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConnector").finish()
    }
}

#[derive(Clone)]
pub(crate) struct TlsAcceptor {
    inner: imp::TlsAcceptor,
}

impl TlsAcceptor {
    pub(crate) fn new(cert: Cert) -> Result<Self, crate::Error> {
        let inner = imp::TlsAcceptor::new(cert)?;
        Ok(Self { inner })
    }

    pub(crate) async fn connect(&self, io: TcpStream) -> Result<imp::TlsStream, crate::Error> {
        self.inner.connect(io).await
    }
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor").finish()
    }
}
