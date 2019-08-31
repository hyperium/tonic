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
use std::pin::Pin;
use std::task::{Context, Poll};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
// #[derive/(Clone)]
pub struct Client {
    svc: Box<
        dyn GrpcService<
                BoxBody,
                ResponseBody = hyper::Body,
                Error = crate::Error,
                Future = BoxFuture<'static, Result<Response<hyper::Body>, crate::Error>>,
            > + Send
            + 'static,
    >,
}

impl Client {
    pub fn connect(addr: Uri) -> Result<Self, super::Error> {
        let settings = Builder::new().http2_only(true).clone();
        let maker = Connect::new(HttpConnector::new(), settings);
        let svc = tower_reconnect::Reconnect::new(maker, addr.clone());

        let svc = AddOrigin::new(svc, addr);
        let svc = BoxService::new(svc);

        Ok(Self { svc: Box::new(svc) })
    }
}

impl GrpcService<BoxBody> for Client {
    type ResponseBody = hyper::Body;
    type Error = super::Error;

    type Future = MapErr<
        BoxFuture<'static, Result<Response<Self::ResponseBody>, crate::Error>>,
        fn(crate::Error) -> super::Error,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.svc
            .poll_ready(cx)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        self.svc
            .call(request)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }
}
