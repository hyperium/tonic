use super::super::BoxFuture;
use super::{grpc_timeout::GrpcTimeout, reconnect::Reconnect, AddOrigin, UserAgent};
use crate::{body::BoxBody, transport::Endpoint};
use http::Uri;
use hyper::client::conn::Builder;
use hyper::client::connect::Connection as HyperConnection;
use hyper::client::service::Connect as HyperConnect;
use std::{
    fmt,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tower::load::Load;
use tower::{
    layer::Layer,
    limit::{concurrency::ConcurrencyLimitLayer, rate::RateLimitLayer},
    util::BoxService,
    ServiceBuilder, ServiceExt,
};
use tower_service::Service;

pub(crate) type Request = http::Request<BoxBody>;
pub(crate) type Response = http::Response<hyper::Body>;

pub(crate) struct Connection {
    inner: BoxService<Request, Response, crate::Error>,
}

impl Connection {
    fn new<C>(connector: C, endpoint: Endpoint, is_lazy: bool) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        let mut settings = Builder::new();
        settings
            .http2_initial_stream_window_size(endpoint.init_stream_window_size)
            .http2_initial_connection_window_size(endpoint.init_connection_window_size)
            .http2_only(true);

        if let Some(val) = endpoint.http2_adaptive_window {
            settings.http2_adaptive_window(val);
        }

        #[cfg(target_arch = "wasm32")]
        {
            settings
                .executor(wasm::Executor)
                // reset streams require `Instant::now` which is not available on wasm
                .http2_max_concurrent_reset_streams(0);
        }

        #[cfg(feature = "transport")]
        {
            settings.http2_keep_alive_interval(endpoint.http2_keep_alive_interval);
            if let Some(val) = endpoint.http2_keep_alive_timeout {
                settings.http2_keep_alive_timeout(val);
            }

            if let Some(val) = endpoint.http2_keep_alive_while_idle {
                settings.http2_keep_alive_while_idle(val);
            }
        }

        let stack = ServiceBuilder::new()
            .layer_fn(|s| AddOrigin::new(s, endpoint.uri.clone()))
            .layer_fn(|s| UserAgent::new(s, endpoint.user_agent.clone()))
            .layer_fn(|s| GrpcTimeout::new(s, endpoint.timeout))
            .option_layer(endpoint.concurrency_limit.map(ConcurrencyLimitLayer::new))
            .option_layer(endpoint.rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .into_inner();

        let connector = HyperConnect::new(connector, settings);
        let conn = Reconnect::new(connector, endpoint.uri.clone(), is_lazy);

        let inner = stack.layer(conn);

        Self {
            inner: BoxService::new(inner),
        }
    }

    pub(crate) async fn connect<C>(connector: C, endpoint: Endpoint) -> Result<Self, crate::Error>
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        Self::new(connector, endpoint, false).ready_oneshot().await
    }

    #[cfg(feature = "transport")]
    pub(crate) fn lazy<C>(connector: C, endpoint: Endpoint) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        Self::new(connector, endpoint, true)
    }
}

impl Service<Request> for Connection {
    type Response = Response;
    type Error = crate::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

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

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::future::Future;
    use std::pin::Pin;

    type BoxSendFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

    pub(crate) struct Executor;

    impl hyper::rt::Executor<BoxSendFuture> for Executor {
        fn execute(&self, fut: BoxSendFuture) {
            wasm_bindgen_futures::spawn_local(fut)
        }
    }
}
