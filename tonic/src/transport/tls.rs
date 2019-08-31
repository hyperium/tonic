use http::Uri;
use hyper::client::connect::HttpConnector;
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{fs, net::TcpStream};
use tokio_rustls::{
    client::TlsStream,
    rustls::{ClientConfig, Session},
    webpki::DNSNameRef,
    TlsConnector as RustlsConnector,
};
use tower_make::MakeConnection;
use tower_service::Service;

const ALPN_H2: &str = "h2";

#[derive(Clone)]
pub struct TlsConnector {
    http: HttpConnector,
    config: Arc<ClientConfig>,
}

impl TlsConnector {
    pub async fn load<P: AsRef<Path>>(ca: P) -> Result<Self, super::Error> {
        let pem = fs::read(ca)
            .await
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e.into())))?;
        Ok(TlsConnector::new(pem))
    }

    fn new(ca: Vec<u8>) -> Self {
        let mut buf = std::io::Cursor::new(ca);

        let mut config = ClientConfig::new();

        config.root_store.add_pem_file(&mut buf).unwrap();
        config.set_protocols(&[Vec::from(&ALPN_H2[..])]);

        let mut http = HttpConnector::new();
        http.enforce_http(false);

        Self {
            http,
            config: Arc::new(config),
        }
    }
}

impl Service<Uri> for TlsConnector {
    type Response = TlsStream<TcpStream>;
    type Error = super::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.http, cx)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e.into())))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let auth = uri.authority_part().unwrap();
        let dns = DNSNameRef::try_from_ascii_str("foo.test.google.fr") //auth.host())
            .unwrap()
            .to_owned();
        let config = self.config.clone();
        let connect = self.http.make_connection(uri.clone());

        let fut = async move {
            let io = match connect.await {
                Ok(io) => io,
                Err(e) => return Err(super::Error::from((super::ErrorKind::Client, e.into()))),
            };

            RustlsConnector::from(config)
                .connect(dns.as_ref(), io)
                .await
                .map_err(|e| {
                    println!("TLS ERROR={:?}", e);
                    super::Error::from((super::ErrorKind::Client, e.into()))
                })
                .and_then(|conn| {
                    let (_, session) = conn.get_ref();
                    let negotiated_protocol = session.get_alpn_protocol();

                    if Some(ALPN_H2.as_bytes()) == negotiated_protocol.as_ref().map(|x| &**x) {
                        Ok(conn)
                    } else {
                        Err(super::Error::from(super::ErrorKind::UnableToNegotiateH2).into())
                    }
                })
        };

        Box::pin(fut)
    }
}
