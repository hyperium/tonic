use super::io::BoxedIo;
use crate::transport::tls::{Cert, TlsAcceptor};
use http::Uri;
use hyper::client::connect::HttpConnector;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_make::MakeConnection;
use tower_service::Service;

type ConnectFuture = <HttpConnector as MakeConnection<Uri>>::Future;

pub struct Connector {
    http: HttpConnector,
    tls: Option<TlsAcceptor>,
}

impl Connector {
    pub fn new(cert: Option<Cert>) -> Result<Self, crate::Error> {
        let mut http = HttpConnector::new();
        http.enforce_http(false);

        let tls = if let Some(cert) = cert {
            Some(TlsAcceptor::new(cert)?)
        } else {
            None
        };

        Ok(Self { http, tls })
    }
}

impl Service<Uri> for Connector {
    type Response = BoxedIo;
    type Error = crate::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeConnection::poll_ready(&mut self.http, cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let io = MakeConnection::make_connection(&mut self.http, uri);
        let tls = self.tls.clone();

        Box::pin(connect(io, tls))
    }
}

async fn connect(
    connect: ConnectFuture,
    tls: Option<TlsAcceptor>,
) -> Result<BoxedIo, crate::Error> {
    let io = connect.await?;

    if let Some(tls) = tls {
        let conn = tls.connect(io).await?;
        Ok(BoxedIo::new(conn))
    } else {
        Ok(BoxedIo::new(io))
    }
}
