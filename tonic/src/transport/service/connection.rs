use super::{layer::ServiceBuilderExt, AddOrigin, Connector};
use crate::{body::BoxBody, transport::Endpoint};
use hyper::client::conn::Builder;
use hyper::client::service::Connect as HyperConnect;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{
    layer::Layer,
    limit::{concurrency::ConcurrencyLimitLayer, rate::RateLimitLayer},
    timeout::TimeoutLayer,
    util::BoxService,
    ServiceBuilder,
};
use tower_load::Load;
use tower_reconnect::Reconnect;
use tower_service::Service;

pub(crate) type Request = http::Request<BoxBody>;
pub(crate) type Response = http::Response<hyper::Body>;

pub struct Connection {
    inner: BoxService<Request, Response, crate::Error>,
}

impl Connection {
    pub fn new(mut endpoint: Endpoint) -> Result<Self, crate::Error> {
        let connector = Connector::new(endpoint.cert.take())?;

        let settings = Builder::new().http2_only(true).clone();

        let stack = ServiceBuilder::new()
            .layer_fn(|s| AddOrigin::new(s, endpoint.uri.clone()))
            .optional_layer(endpoint.timeout.map(|t| TimeoutLayer::new(t)))
            .optional_layer(
                endpoint
                    .concurrency_limit
                    .map(|l| ConcurrencyLimitLayer::new(l)),
            )
            .optional_layer(endpoint.rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .into_inner();

        let conn = Reconnect::new(HyperConnect::new(connector, settings), endpoint.uri.clone());

        let inner = stack.layer(conn);

        Ok(Self {
            inner: BoxService::new(inner),
        })
    }
}

impl Service<Request> for Connection {
    type Response = Response;
    type Error = crate::Error;

    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.inner.call(req)
    }
}

impl Load for Connection {
    type Metric = usize;

    fn load(&self) -> Self::Metric {
        0
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish()
    }
}
