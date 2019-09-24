use super::{
    service::{BoxService, Connection, ServiceList},
    Endpoint,
};
use crate::{body::BoxBody, client::GrpcService};
use futures_util::try_future::{MapErr, TryFutureExt};
use hyper::{Request, Response};
use std::{
    convert::TryInto,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::buffer::{future::ResponseFuture, Buffer};
use tower::discover::Discover;
use tower_balance::p2c::Balance;
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

/// A default batteries included `transport` channel.
///
/// This provides a fully featured http2 gRPC client based on [`hyper::Client`]
/// and `tower` services.
#[derive(Clone)]
pub struct Channel {
    svc: Buffer<Inner, Request<BoxBody>>,
}

impl Channel {
    /// Create a [`Builder`] that can create a [`Channel`].
    pub fn builder() -> Builder {
        Builder::new()
    }
}

impl GrpcService<BoxBody> for Channel {
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

#[derive(Debug)]
pub struct Builder<D = ServiceList> {
    ca: Option<Vec<u8>>,
    override_domain: Option<String>,
    buffer_size: usize,
    balance: Option<D>,
}

impl Builder {
    fn new() -> Self {
        Self {
            ca: None,
            override_domain: None,
            buffer_size: 1024,
            balance: None,
        }
    }

    /// Set the buffer size for when the inner client applies back pressure and
    /// can no longer accept requests. Defaults to `1024`.
    pub fn buffer(&mut self, size: usize) -> &mut Self {
        self.buffer_size = size;
        self
    }

    pub fn balance_list(&mut self, list: Vec<Endpoint>) -> Result<Channel, super::Error> {
        let discover = ServiceList::new(list);
        self.balance(discover)
    }

    fn balance<D>(&mut self, discover: D) -> Result<Channel, super::Error>
    where
        D: Discover<Service = Connection> + Unpin + Send + 'static,
        D::Error: Into<crate::Error>,
        D::Key: Send + Clone,
    {
        let svc = Balance::from_entropy(discover);

        let svc = BoxService::new(svc);
        let svc = Buffer::new(Box::new(svc) as Inner, 100);

        Ok(Channel { svc })
    }

    pub fn connect(&mut self, endpoint: Endpoint) -> Result<Channel, super::Error> {
        self.balance_list(vec![endpoint])
    }

    pub fn build<T>(&mut self, uri: T) -> Result<Channel, super::Error>
    where
        T: TryInto<Endpoint>,
        T::Error: Into<crate::Error>,
    {
        let uri = uri
            .try_into()
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e.into())))?;

        self.balance_list(vec![uri.into()])
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel").finish()
    }
}
