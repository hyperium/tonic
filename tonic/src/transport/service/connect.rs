use super::{add_origin::AddOrigin, connector::Connector};
use crate::{transport::Endpoint, BoxBody};
use http::{Request, Response, Uri};
use hyper::client::conn::Builder;
use hyper::client::service::Connect as HyperConnect;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_load::Load;
use tower_reconnect::Reconnect;
use tower_service::Service;

pub struct Connection {
    inner: AddOrigin<Reconnect<HyperConnect<Connector, BoxBody, Uri>, Uri>>,
}

impl Connection {
    pub fn new(mut endpoint: Endpoint) -> Result<Self, crate::Error> {
        let connector = Connector::new(endpoint.take_cert())?;

        let settings = Builder::new().http2_only(true).clone();
        let connect = HyperConnect::new(connector, settings);
        let reconnect = Reconnect::new(connect, endpoint.uri().clone());
        let inner = AddOrigin::new(reconnect, endpoint.uri().clone());

        Ok(Self { inner })
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

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Connection").finish()
    }
}
