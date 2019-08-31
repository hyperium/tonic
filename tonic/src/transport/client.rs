use crate::{
    body::BoxBody,
    service::{AddOrigin, GrpcService},
};
use http::Uri;
use hyper::client::conn;
use hyper::{Request, Response};
use std::task::{Context, Poll};
use tower_service::Service;
use hyper::client::conn::Builder;
use hyper::client::connect::HttpConnector;
use hyper::client::service::{Connect, MakeService};

//type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type BoxService = Box<
    dyn GrpcService<
            BoxBody,
            ResponseBody = hyper::Body,
            Error = hyper::Error,
            Future = conn::ResponseFuture, //BoxFuture<'static, Result<Response<hyper::Body>, hyper::Error>>,
        > + Send
        + 'static,
>;

// #[derive/(Clone)]
pub struct Client {
    svc: BoxService,
}

impl Client {
    pub async fn connect(addr: Uri) -> Result<Self, hyper::Error> {
        let settings = Builder::new().http2_only(true).clone();
        let mut maker = Connect::new(HttpConnector::new(), settings);

        maker.make_service(addr.clone()).await.map(|svc| Self::new(addr, svc))
    }

    fn new<S>(addr: Uri, service: S) -> Self
    where
        S: Service<
                Request<BoxBody>,
                Response = Response<hyper::Body>,
                Error = hyper::Error,
                Future = conn::ResponseFuture, //BoxFuture<'static, Result<Response<hyper::Body>, hyper::Error>>,
            > + Send
            + 'static,
    {
        let svc = AddOrigin::new(service, addr);

        Self { svc: Box::new(svc) }
    }
}

impl GrpcService<BoxBody> for Client {
    type ResponseBody = hyper::Body;
    type Error = hyper::Error;

    // type Future = BoxFuture<'static, Result<Response<Self::ResponseBody>, Self::Error>>;
    type Future = conn::ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.svc.poll_ready(cx)
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        self.svc.call(request)
    }
}
