use super::{AddOrigin, Reconnect, SharedExec, UserAgent};
use crate::{
    body::{boxed, BoxBody},
    transport::{channel::BoxFuture, service::GrpcTimeout, Endpoint},
};
use http::Uri;
use hyper::rt;
use hyper::{client::conn::http2::Builder, rt::Executor};
use hyper_util::rt::TokioTimer;
use std::{
    fmt,
    task::{Context, Poll},
};
use tower::load::Load;
use tower::{
    layer::Layer,
    limit::{concurrency::ConcurrencyLimitLayer, rate::RateLimitLayer},
    util::BoxService,
    ServiceBuilder, ServiceExt,
};
use tower_service::Service;

pub(crate) type Response<B = BoxBody> = http::Response<B>;
pub(crate) type Request<B = BoxBody> = http::Request<B>;

pub(crate) struct Connection {
    inner: BoxService<Request, Response, crate::Error>,
}

impl Connection {
    fn new<C>(connector: C, endpoint: Endpoint, is_lazy: bool) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        let mut settings: Builder<SharedExec> = Builder::new(endpoint.executor.clone())
            .initial_stream_window_size(endpoint.init_stream_window_size)
            .initial_connection_window_size(endpoint.init_connection_window_size)
            .keep_alive_interval(endpoint.http2_keep_alive_interval)
            .timer(TokioTimer::new())
            .clone();

        if let Some(val) = endpoint.http2_keep_alive_timeout {
            settings.keep_alive_timeout(val);
        }

        if let Some(val) = endpoint.http2_keep_alive_while_idle {
            settings.keep_alive_while_idle(val);
        }

        if let Some(val) = endpoint.http2_adaptive_window {
            settings.adaptive_window(val);
        }

        let stack = ServiceBuilder::new()
            .layer_fn(|s| {
                let origin = endpoint.origin.as_ref().unwrap_or(&endpoint.uri).clone();

                AddOrigin::new(s, origin)
            })
            .layer_fn(|s| UserAgent::new(s, endpoint.user_agent.clone()))
            .layer_fn(|s| GrpcTimeout::new(s, endpoint.timeout))
            .option_layer(endpoint.concurrency_limit.map(ConcurrencyLimitLayer::new))
            .option_layer(endpoint.rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .into_inner();

        let make_service =
            MakeSendRequestService::new(connector, endpoint.executor.clone(), settings);

        let conn = Reconnect::new(make_service, endpoint.uri.clone(), is_lazy);

        Self {
            inner: BoxService::new(stack.layer(conn)),
        }
    }

    pub(crate) async fn connect<C>(connector: C, endpoint: Endpoint) -> Result<Self, crate::Error>
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        Self::new(connector, endpoint, false).ready_oneshot().await
    }

    pub(crate) fn lazy<C>(connector: C, endpoint: Endpoint) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        Self::new(connector, endpoint, true)
    }
}

impl Service<Request> for Connection {
    type Response = Response;
    type Error = crate::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

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

struct SendRequest {
    inner: hyper::client::conn::http2::SendRequest<BoxBody>,
}

impl From<hyper::client::conn::http2::SendRequest<BoxBody>> for SendRequest {
    fn from(inner: hyper::client::conn::http2::SendRequest<BoxBody>) -> Self {
        Self { inner }
    }
}

impl tower::Service<http::Request<BoxBody>> for SendRequest {
    type Response = http::Response<BoxBody>;
    type Error = crate::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let fut = self.inner.send_request(req);

        Box::pin(async move { fut.await.map_err(Into::into).map(|res| res.map(boxed)) })
    }
}

struct MakeSendRequestService<C> {
    connector: C,
    executor: SharedExec,
    settings: Builder<SharedExec>,
}

impl<C> MakeSendRequestService<C> {
    fn new(connector: C, executor: SharedExec, settings: Builder<SharedExec>) -> Self {
        Self {
            connector,
            executor,
            settings,
        }
    }
}

impl<C> tower::Service<Uri> for MakeSendRequestService<C>
where
    C: Service<Uri> + Send + 'static,
    C::Error: Into<crate::Error> + Send,
    C::Future: Unpin + Send,
    C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
{
    type Response = SendRequest;
    type Error = crate::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.connector.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let fut = self.connector.call(req);
        let builder = self.settings.clone();
        let executor = self.executor.clone();

        Box::pin(async move {
            let io = fut.await.map_err(Into::into)?;
            let (send_request, conn) = builder.handshake(io).await?;

            Executor::<BoxFuture<'static, ()>>::execute(
                &executor,
                Box::pin(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!("connection task error: {:?}", e);
                    }
                }) as _,
            );

            Ok(SendRequest::from(send_request))
        })
    }
}
