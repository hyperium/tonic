use http::Uri;
use hyper::client::connect::HttpConnector;
use openssl::ssl::{SslConnector, SslMethod};
use openssl::x509::X509;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::TcpStream;
use tokio_openssl::{connect, SslStream};
use tower_make::MakeConnection;
use tower_service::Service;

#[derive(Clone)]
pub struct TlsConnector {
    http: HttpConnector,
    config: SslConnector,
    domain: String,
}

impl TlsConnector {
    pub fn new(ca: Vec<u8>, domain: String) -> Result<Self, super::Error> {
        let mut config = SslConnector::builder(SslMethod::tls()).unwrap();

        config.set_alpn_protos(b"\x02h2").unwrap();

        let ca = X509::from_pem(&ca[..]).unwrap();

        config.cert_store_mut().add_cert(ca).unwrap();

        let config = config.build();

        let mut http = HttpConnector::new();
        http.enforce_http(false);

        Ok(Self {
            http,
            config,
            domain,
        })
    }
}

impl Service<Uri> for TlsConnector {
    type Response = SslStream<TcpStream>;
    type Error = super::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.http, cx)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e.into())))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let config = self.config.configure().unwrap();
        let tcp = self.http.make_connection(uri.clone());
        let domain = self.domain.clone();

        let fut = async move {
            let io = tcp.await.unwrap();
            let tls = connect(config, &domain, io).await.unwrap();
            Ok(tls)
        };

        Box::pin(fut)
    }
}
