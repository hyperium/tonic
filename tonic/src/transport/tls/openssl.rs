use super::Cert;
use openssl::ssl::{SslConnector, SslMethod};
use openssl::x509::X509;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

const ALPN_H2: &[u8] = b"\x02h2";

pub type TlsStream = SslStream<TcpStream>;

#[derive(Clone)]
pub struct TlsAcceptor {
    config: SslConnector,
    domain: Arc<String>,
}

impl TlsAcceptor {
    pub fn new(cert: Cert) -> Result<Self, crate::Error> {
        let Cert { ca, domain } = cert;
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
