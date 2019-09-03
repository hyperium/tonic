use super::Cert;
use openssl::ssl::{SslAcceptor, SslConnector, SslMethod};
use openssl::{pkey::PKey, x509::X509};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

const ALPN_H2: &[u8] = b"\x02h2";

pub type TlsStream = SslStream<TcpStream>;

#[derive(Clone)]
pub struct TlsConnector {
    config: SslConnector,
    domain: Arc<String>,
}

impl TlsConnector {
    pub fn new(cert: Cert) -> Result<Self, crate::Error> {
        let Cert { ca, domain, .. } = cert;
        let mut config = SslConnector::builder(SslMethod::tls()).unwrap();

        config.set_alpn_protos(ALPN_H2)?;

        let ca = X509::from_pem(&ca[..])?;

        config.cert_store_mut().add_cert(ca)?;

        let config = config.build();

        Ok(Self {
            config,
            domain: Arc::new(domain),
        })
    }

    pub async fn connect(&self, io: TcpStream) -> Result<TlsStream, crate::Error> {
        let config = self.config.configure()?;
        let tls = tokio_openssl::connect(config, &self.domain, io).await?;
        Ok(tls)
    }
}

#[derive(Clone)]
pub struct TlsAcceptor {
    config: SslAcceptor,
}

impl TlsAcceptor {
    pub fn new(cert: Cert) -> Result<Self, crate::Error> {
        let Cert { ca, key, .. } = cert;

        let key = PKey::private_key_from_pem(&key.unwrap()[..])?;
        let ca = X509::from_pem(&ca[..])?;

        let mut config = SslAcceptor::mozilla_modern(SslMethod::tls())?;

        config.set_private_key(&key)?;
        config.set_certificate(&ca)?;

        Ok(Self {
            config: config.build(),
        })
    }

    pub async fn connect(&self, io: TcpStream) -> Result<TlsStream, crate::Error> {
        let config = self.config.clone();
        let tls = tokio_openssl::accept(&config, io).await?;
        Ok(tls)
    }
}
