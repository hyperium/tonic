use super::io::BoxedIo;
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
}

impl Connector {
    pub fn new() -> Self {
        Self {
            http: HttpConnector::new(),
        }
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
        let connect_fut = MakeConnection::make_connection(&mut self.http, uri);

        Box::pin(connect(connect_fut))
    }
}

async fn connect(connect: ConnectFuture) -> Result<BoxedIo, crate::Error> {
    let io = connect.await?;

    // TODO: build tls based on creds and features

    Ok(BoxedIo::new(io))
}
