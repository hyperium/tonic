use super::tls::TlsConnector;
use crate::{
    body::BoxBody,
    service::{AddOrigin, BoxService, GrpcService},
};
use futures_util::try_future::{MapErr, TryFutureExt};
use http::Uri;
use hyper::client::conn::Builder;
use hyper::client::connect::HttpConnector;
use hyper::client::service::Connect;
use hyper::{Request, Response};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_buffer::{future::ResponseFuture, Buffer};
use tower_service::Service;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Inner = Box<
    dyn Service<
            Request<BoxBody>,
            Response = Response<hyper::Body>,
            Error = crate::Error,
            Future = BoxFuture<'static, Result<Response<hyper::Body>, crate::Error>>,
        > + Send
        + 'static,
>;

#[derive(Clone)]
pub struct Client {
    svc: Buffer<Inner, Request<BoxBody>>,
}

impl Client {
    pub fn connect(addr: Uri) -> Result<Self, super::Error> {
        let settings = Builder::new().http2_only(true).clone();
        let maker = Connect::new(HttpConnector::new(), settings);
        let svc = tower_reconnect::Reconnect::new(maker, addr.clone());

        let svc = AddOrigin::new(svc, addr);
        let svc = BoxService::new(svc);

        let svc = Buffer::new(Box::new(svc) as Inner, 100);

        Ok(Self { svc })
    }

    pub async fn connect_with_tls<P: AsRef<Path>>(addr: Uri, ca: P) -> Result<Self, super::Error> {
        let settings = Builder::new().http2_only(true).clone();

        let tls_connector = TlsConnector::load(ca).await?;

        let maker = Connect::new(tls_connector, settings);
        let svc = tower_reconnect::Reconnect::new(maker, addr.clone());

        let svc = AddOrigin::new(svc, addr);
        let svc = BoxService::new(svc);

        let svc = Buffer::new(Box::new(svc) as Inner, 100);

        Ok(Self { svc })
    }
}

impl GrpcService<BoxBody> for Client {
    type ResponseBody = hyper::Body;
    type Error = super::Error;

    type Future = MapErr<
        ResponseFuture<BoxFuture<'static, Result<Response<Self::ResponseBody>, crate::Error>>>,
        fn(crate::Error) -> super::Error,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        GrpcService::poll_ready(&mut self.svc, cx)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        GrpcService::call(&mut self.svc, request)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }
}
