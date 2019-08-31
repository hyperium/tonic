use http::Uri;
use hyper::client::connect::HttpConnector;
use openssl::ssl::{ConnectConfiguration, SslConnector, SslMethod};
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{fs, net::TcpStream};
use tokio_openssl::{connect, SslStream};
use tower_make::MakeConnection;
use tower_service::Service;

const ALPN_H2: &str = "h2";

#[derive(Clone)]
pub struct TlsConnector {
    http: HttpConnector,
    config: SslConnector,
}

impl TlsConnector {
    pub async fn load<P: AsRef<Path>>(ca: P) -> Result<Self, super::Error> {
        let mut config = SslConnector::builder(SslMethod::tls()).unwrap();

        config.set_alpn_protos(ALPN_H2.as_bytes()).unwrap();

        config.set_ca_file(ca).unwrap();

        let config = config.build();

        let mut http = HttpConnector::new();
        http.enforce_http(false);

        Ok(Self {
            http,
            config,
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

        let fut = async move {
            let io = tcp.await.unwrap();
            let domain = "foo.test.google.fr";
            let tls = connect(config, &domain, io).await.unwrap();
            Ok(tls)
        };

        Box::pin(fut)
    }
}
