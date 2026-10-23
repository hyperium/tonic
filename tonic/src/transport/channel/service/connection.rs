/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use super::{AddOrigin, Change, Reconnect, SharedExec, UserAgent};
use crate::{
    body::Body,
    transport::{Endpoint, channel::BoxFuture, service::GrpcTimeout},
};
use http::{Request, Response, Uri};
use hyper::rt;
use hyper::{client::conn::http2::Builder, rt::Executor};
use hyper_util::rt::TokioTimer;
use std::{
    fmt,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;
use tower::load::Load;
use tower::{
    ServiceBuilder, ServiceExt,
    layer::Layer,
    limit::{concurrency::ConcurrencyLimitLayer, rate::RateLimitLayer},
    util::BoxService,
};
use tower_service::Service;

/// How long to wait before re-inserting a balanced endpoint that was evicted after a
/// failed connection attempt. See [`ReinsertOnError`].
const REINSERT_BACKOFF: Duration = Duration::from_secs(1);

pub(crate) struct Connection {
    inner: BoxService<Request<Body>, Response<Body>, crate::BoxError>,
}

impl Connection {
    fn new<C>(connector: C, endpoint: Endpoint, is_lazy: bool, fail_early: bool) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::BoxError> + Send,
        C::Future: Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        let mut settings: Builder<SharedExec> = Builder::new(endpoint.executor.clone())
            .initial_stream_window_size(endpoint.init_stream_window_size)
            .initial_connection_window_size(endpoint.init_connection_window_size)
            .keep_alive_interval(endpoint.http2_keep_alive_interval)
            .timer(TokioTimer::new())
            .clone();

        if let Some(val) = endpoint.max_frame_size {
            settings.max_frame_size(val);
        }

        if let Some(val) = endpoint.http2_keep_alive_timeout {
            settings.keep_alive_timeout(val);
        }

        if let Some(val) = endpoint.http2_keep_alive_while_idle {
            settings.keep_alive_while_idle(val);
        }

        if let Some(val) = endpoint.http2_adaptive_window {
            settings.adaptive_window(val);
        }

        if let Some(val) = endpoint.http2_header_table_size {
            settings.header_table_size(val);
        }

        if let Some(val) = endpoint.http2_max_header_list_size {
            settings.max_header_list_size(val);
        }

        let stack = ServiceBuilder::new()
            .layer_fn(|s| {
                let origin = endpoint.origin.as_ref().unwrap_or(endpoint.uri()).clone();

                AddOrigin::new(s, origin)
            })
            .layer_fn(|s| UserAgent::new(s, endpoint.user_agent.clone()))
            .layer_fn(|s| GrpcTimeout::new(s, endpoint.timeout))
            .option_layer(endpoint.concurrency_limit.map(ConcurrencyLimitLayer::new))
            .option_layer(endpoint.rate_limit.map(|(l, d)| RateLimitLayer::new(l, d)))
            .into_inner();

        let make_service =
            MakeSendRequestService::new(connector, endpoint.executor.clone(), settings);

        let conn = Reconnect::new(make_service, endpoint.uri().clone(), is_lazy, fail_early);

        Self {
            inner: BoxService::new(stack.layer(conn)),
        }
    }

    pub(crate) async fn connect<C>(
        connector: C,
        endpoint: Endpoint,
    ) -> Result<Self, crate::BoxError>
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::BoxError> + Send,
        C::Future: Unpin + Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        let fail_early = endpoint.eager_connect_errors;
        Self::new(connector, endpoint, false, fail_early)
            .ready_oneshot()
            .await
    }

    pub(crate) fn lazy<C>(connector: C, endpoint: Endpoint) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::BoxError> + Send,
        C::Future: Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
    {
        let fail_early = endpoint.eager_connect_errors;
        Self::new(connector, endpoint, true, fail_early)
    }

    /// Like [`Connection::lazy`], but for connections managed by a discovery-driven
    /// [`tower::balance::p2c::Balance`].
    ///
    /// `Balance` polls its services through a `tower::ready_cache::ReadyCache`, which
    /// permanently evicts any service whose `poll_ready` returns `Err` and never
    /// retries it on its own. Since [`Endpoint::eager_connect_errors`] intentionally
    /// causes connect failures to surface from `poll_ready` (so `Balance` can skip a
    /// broken endpoint instead of routing a call to it), a connection built here is
    /// wrapped in [`ReinsertOnError`] so that an eviction like that is followed by an
    /// automatic re-insert of the same key after a short backoff, instead of the
    /// endpoint being lost until the caller notices and re-inserts it manually.
    pub(crate) fn lazy_for_discovery<C, K>(
        connector: C,
        endpoint: Endpoint,
        key: K,
        reinsert: mpsc::Sender<Change<K, Endpoint>>,
    ) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::BoxError> + Send,
        C::Future: Send,
        C::Response: rt::Read + rt::Write + Unpin + Send + 'static,
        K: Clone + Send + 'static,
    {
        let fail_early = endpoint.eager_connect_errors;
        let executor = endpoint.executor.clone();
        let retry_endpoint = endpoint.clone();
        let inner = Self::new(connector, endpoint, true, fail_early);

        Self {
            inner: BoxService::new(ReinsertOnError {
                inner,
                key,
                endpoint: retry_endpoint,
                executor,
                reinsert,
            }),
        }
    }
}

/// Wraps a discovery-managed [`Connection`] so that a `poll_ready` error (which causes
/// `tower`'s `Balance`/`ReadyCache` to evict it permanently, see [`Connection::lazy_for_discovery`])
/// is followed by scheduling a fresh [`Change::Insert`] for the same key after
/// [`REINSERT_BACKOFF`], so the endpoint can rejoin the balancer on its own.
struct ReinsertOnError<K> {
    inner: Connection,
    key: K,
    endpoint: Endpoint,
    executor: SharedExec,
    reinsert: mpsc::Sender<Change<K, Endpoint>>,
}

impl<K> ReinsertOnError<K>
where
    K: Clone + Send + 'static,
{
    fn schedule_reinsert(&self) {
        let key = self.key.clone();
        let endpoint = self.endpoint.clone();
        let reinsert = self.reinsert.clone();

        Executor::<BoxFuture<'static, ()>>::execute(
            &self.executor,
            Box::pin(async move {
                tokio::time::sleep(REINSERT_BACKOFF).await;
                let _ = reinsert.send(Change::Insert(key, endpoint)).await;
            }) as _,
        );
    }
}

impl<K> Service<Request<Body>> for ReinsertOnError<K>
where
    K: Clone + Send + 'static,
{
    type Response = Response<Body>;
    type Error = crate::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let poll = Service::poll_ready(&mut self.inner, cx);
        if let Poll::Ready(Err(_)) = &poll {
            self.schedule_reinsert();
        }
        poll
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.inner.call(req)
    }
}

impl Service<Request<Body>> for Connection {
    type Response = Response<Body>;
    type Error = crate::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
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
    inner: hyper::client::conn::http2::SendRequest<Body>,
}

impl From<hyper::client::conn::http2::SendRequest<Body>> for SendRequest {
    fn from(inner: hyper::client::conn::http2::SendRequest<Body>) -> Self {
        Self { inner }
    }
}

impl tower::Service<Request<Body>> for SendRequest {
    type Response = Response<Body>;
    type Error = crate::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let fut = self.inner.send_request(req);

        Box::pin(async move { fut.await.map_err(Into::into).map(|res| res.map(Body::new)) })
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
    C::Error: Into<crate::BoxError> + Send,
    C::Future: Send,
    C::Response: rt::Read + rt::Write + Unpin + Send,
{
    type Response = SendRequest;
    type Error = crate::BoxError;
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
