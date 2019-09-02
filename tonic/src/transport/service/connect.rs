use super::{add_origin::AddOrigin, connector::Connector};
use crate::body::BoxBody;
use http::{Request, Response, Uri};
use hyper::client::conn::Builder;
use hyper::client::service::Connect as HyperConnect;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_load::Load;
use tower_reconnect::Reconnect;
use tower_service::Service;

pub struct Connection {
    inner: AddOrigin<Reconnect<HyperConnect<Connector, BoxBody, Uri>, Uri>>,
}

impl Connection {
    pub fn new(uri: Uri) -> Self {
        let connector = Connector::new();
        let settings = Builder::new().http2_only(true).clone();
        let connect = HyperConnect::new(connector, settings);
        let reconnect = Reconnect::new(connect, uri.clone());
        let inner = AddOrigin::new(reconnect, uri);

        Self { inner }
    }
}

impl Service<Request<BoxBody>> for Connection {
    type Response = Response<hyper::Body>;
    type Error = crate::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<BoxBody>) -> Self::Future {
        let fut = self.inner.call(req);
        Box::pin(fut)
    }
}

impl Load for Connection {
    type Metric = usize;

    fn load(&self) -> Self::Metric {
        0
    }
}
