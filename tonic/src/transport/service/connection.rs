use super::{layer::ServiceBuilderExt, reconnect::Reconnect, AddOrigin};
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
use tower_service::Service;

pub(crate) type Request = http::Request<BoxBody>;
pub(crate) type Response = http::Response<hyper::Body>;

pub(crate) struct Connection {
    inner: BoxService<Request, Response, crate::Error>,
}

impl Connection {
    pub(crate) async fn new<C>(endpoint: Endpoint<C>) -> Result<Self, crate::Error>
    where
        C: tower_make::MakeConnection<hyper::Uri> + Send + 'static,
        C::Connection: Unpin + Send + 'static,
        C::Future: Send + 'static,
        C::Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    {
        let Endpoint {
            uri,
            init_stream_window_size,
            init_connection_window_size,
            timeout,
            concurrency_limit,
            rate_limit,
            connector,
            ..
        } = endpoint;

        let settings = Builder::new()
            .http2_initial_stream_window_size(init_stream_window_size)
            .http2_initial_connection_window_size(init_connection_window_size)
            .http2_only(true)
            .clone();

        let mut connector = HyperConnect::new(connector, settings);
        let initial_conn = connector.call(uri.clone()).await?;
        let conn = Reconnect::new(initial_conn, connector, uri.clone());

        let stack = ServiceBuilder::new()
            .layer_fn(|s| AddOrigin::new(s, uri.clone()))
            .optional_layer(timeout.map(TimeoutLayer::new))
            .optional_layer(concurrency_limit.map(ConcurrencyLimitLayer::new))
            .optional_layer(rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .into_inner();

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
