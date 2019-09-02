// #[cfg(feature = "openssl-1")]
// #[cfg(not(feature = "rustls"))]
// #[path = "rustls.rs"]
// mod imp;

#[cfg(feature = "native-tls")]
#[cfg(not(feature = "rustls"))]
#[path = "openssl.rs"]
mod imp;

use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub struct Cert {
    pub(crate) ca: Vec<u8>,
    pub(crate) domain: String,
}

#[derive(Clone)]
pub struct TlsAcceptor {
    inner: imp::TlsAcceptor,
}

impl TlsAcceptor {
    pub fn new(cert: Cert) -> Result<Self, crate::Error> {
        let inner = imp::TlsAcceptor::new(cert)?;
        Ok(Self { inner })
    }

    pub async fn connect(&self, io: TcpStream) -> Result<imp::TlsStream, crate::Error> {
        self.inner.connect(io).await
    }
}
