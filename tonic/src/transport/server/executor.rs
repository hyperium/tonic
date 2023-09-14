use std::{convert::Infallible, future::Future};

use bytes::Bytes;
use http::{Request, Response};
use hyper::server::accept;
use hyper::Body;
use std::marker::PhantomData;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_stream::Stream;
use tower::Layer;
use tower_service::Service;

use crate::{
    body::{BoxBody, LocalBoxBody},
    transport::{LocalExec, TokioExec},
    util::{body::HasBoxedBodyWithMapErr, BoxCloneService},
};

const DEFAULT_HTTP2_KEEPALIVE_TIMEOUT_SECS: u64 = 20;

pub trait HasBoxCloneService {
    type BoxCloneService: BoxCloneService;
}

impl HasBoxCloneService for TokioExec {
    type BoxCloneService =
        tower::util::BoxCloneService<Request<Body>, Response<BoxBody>, Infallible>;
}

impl HasBoxCloneService for LocalExec {
    type BoxCloneService =
        crate::util::LocalBoxCloneService<Request<Body>, Response<LocalBoxBody>, Infallible>;
}

pub trait HasBoxedCloneService<S>: HasBoxCloneService {
    fn boxed_clone_service(svc: S) -> Self::BoxCloneService;
}

impl<S> HasBoxedCloneService<S> for TokioExec
where
    S: Service<Request<Body>, Response = Response<BoxBody>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    fn boxed_clone_service(svc: S) -> Self::BoxCloneService {
        Self::BoxCloneService::new(svc)
    }
}

impl<S> HasBoxedCloneService<S> for LocalExec
where
    S: Service<Request<Body>, Response = Response<LocalBoxBody>, Error = Infallible>
        + Clone
        + 'static,
    S::Future: 'static,
{
    fn boxed_clone_service(svc: S) -> Self::BoxCloneService {
        Self::BoxCloneService::new(svc)
    }
}

/// An executor trait for `super::SvcFuture`.
pub trait HasBoxedHttpBody<F, ResBody>: HasBoxedBodyWithMapErr<ResBody> {
    type BoxHttpBody;

    fn boxed_http_body(body: ResBody) -> Self::BoxHttpBody;
}

impl<F, ResBody> HasBoxedHttpBody<F, ResBody> for TokioExec
where
    Self: HasBoxedBodyWithMapErr<ResBody>,
    ResBody: http_body::Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type BoxHttpBody = http_body::combinators::UnsyncBoxBody<Bytes, crate::Error>;

    fn boxed_http_body(body: ResBody) -> Self::BoxHttpBody {
        Self::BoxHttpBody::new(body.map_err(Into::into))
    }
}

impl<F, ResBody> HasBoxedHttpBody<F, ResBody> for LocalExec
where
    Self: HasBoxedBodyWithMapErr<ResBody>,
    ResBody: http_body::Body<Data = Bytes> + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type BoxHttpBody = crate::body::UnsendBoxBody<Bytes, crate::Error>;

    fn boxed_http_body(body: ResBody) -> Self::BoxHttpBody {
        Self::BoxHttpBody::new(body.map_err(Into::into))
    }
}

/// An executor trait for `super::MakeSvc`.
pub trait HttpServiceExecutor<S, ResBody>:
    HasBoxCloneService + HasBoxedBodyWithMapErr<ResBody> + HasBoxedHttpBody<S::Future, ResBody>
where
    S: Service<Request<Body>, Response = Response<ResBody>>,
{
    type BoxService;
}

impl<S, ResBody> HttpServiceExecutor<S, ResBody> for TokioExec
where
    Self: HasBoxedBodyWithMapErr<ResBody>,
    S: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
    ResBody: http_body::Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type BoxService =
        tower::util::UnsyncBoxService<Request<Body>, Response<Self::BoxHttpBody>, crate::Error>;
}

impl<S, ResBody> HttpServiceExecutor<S, ResBody> for LocalExec
where
    Self: HasBoxedBodyWithMapErr<ResBody>,
    S: Service<Request<Body>, Response = Response<ResBody>> + Clone + 'static,
    S::Future: 'static,
    S::Error: Into<crate::Error> + Send,
    ResBody: http_body::Body<Data = Bytes> + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type BoxService =
        tower::util::UnsyncBoxService<Request<Body>, Response<Self::BoxHttpBody>, crate::Error>;
}

/// An executor trait for `super::Server`.
#[allow(missing_docs, missing_debug_implementations)]
pub trait ServeWithShutdown<L, S, I, F, IO, IE, ResBody>: Sized {
    type BoxFuture: Future<Output = Result<(), crate::transport::Error>>;
    fn serve_with_shutdown(
        server: super::Server<Self, L>,
        svc: S,
        incoming: I,
        signal: Option<F>,
    ) -> Self::BoxFuture;
}

macro_rules! define_serve_with_shutdown {
($exec: ty, $box_future: tt $(, $maybe_send: tt)?) => {

impl<L, S, I, F, IO, IE, ResBody> ServeWithShutdown<L, S, I, F, IO, IE, ResBody> for $exec
where
    IO: AsyncRead + AsyncWrite + super::Connected + Unpin + Send + 'static,
    ResBody: http_body::Body<Data = Bytes> + $($maybe_send +)* 'static,
    ResBody::Error: Into<crate::Error>,
    L: Layer<S> $(+ $maybe_send)* + 'static,
    L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + $($maybe_send +)* 'static,
    <L::Service as Service<Request<Body>>>::Future:  $($maybe_send +)* 'static,
    <L::Service as Service<Request<Body>>>::Error: Into<crate::Error> + Send,
    I: Stream<Item = Result<IO, IE>> $(+ $maybe_send)* +'static,
    IO: AsyncRead + AsyncWrite + super::Connected + Unpin + Send + 'static,
    IO::ConnectInfo: Clone + Send + Sync + 'static,
    IE: Into<crate::Error> + $($maybe_send +)* 'static,
    F: Future<Output = ()> + $($maybe_send +)* 'static,
    ResBody: http_body::Body<Data = Bytes>,
{
    type BoxFuture = crate::codegen::$box_future<(), crate::transport::Error>;

    fn serve_with_shutdown(
        server: super::Server<Self, L>,
        svc: S,
        incoming: I,
        signal: Option<F>,
    ) -> Self::BoxFuture {
        let trace_interceptor = server.trace_interceptor.clone();
        let concurrency_limit = server.concurrency_limit;
        let init_connection_window_size = server.init_connection_window_size;
        let init_stream_window_size = server.init_stream_window_size;
        let max_concurrent_streams = server.max_concurrent_streams;
        let timeout = server.timeout;
        let max_frame_size = server.max_frame_size;
        let http2_only = !server.accept_http1;

        let http2_keepalive_interval = server.http2_keepalive_interval;
        let http2_keepalive_timeout = server
            .http2_keepalive_timeout
            .unwrap_or_else(|| Duration::new(DEFAULT_HTTP2_KEEPALIVE_TIMEOUT_SECS, 0));
        let http2_adaptive_window = server.http2_adaptive_window;
        let http2_max_pending_accept_reset_streams = server.http2_max_pending_accept_reset_streams;

        let svc = server.service_builder.service(svc);
        let exec = server.exec.clone();

        let tcp = super::incoming::tcp_incoming(incoming, server);
        let incoming = accept::from_stream::<_, _, crate::Error>(tcp);

        let svc = super::MakeSvc {
            inner: svc,
            concurrency_limit,
            timeout,
            trace_interceptor,
            _marker: PhantomData::<(Self, IO)>,
        };

        let builder = hyper::Server::builder(incoming)
            .executor(exec)
            .http2_only(http2_only)
            .http2_initial_connection_window_size(init_connection_window_size)
            .http2_initial_stream_window_size(init_stream_window_size)
            .http2_max_concurrent_streams(max_concurrent_streams)
            .http2_keep_alive_interval(http2_keepalive_interval)
            .http2_keep_alive_timeout(http2_keepalive_timeout)
            .http2_adaptive_window(http2_adaptive_window.unwrap_or_default())
            .http2_max_pending_accept_reset_streams(http2_max_pending_accept_reset_streams)
            .http2_max_frame_size(max_frame_size);

        Box::pin(async move {
            if let Some(signal) = signal {
                builder
                    .serve(svc)
                    .with_graceful_shutdown(signal)
                    .await
                    .map_err(crate::transport::Error::from_source)?;
            } else {
                builder.serve(svc).await.map_err(crate::transport::Error::from_source)?;
            }

            Ok(())
        })
    }
}

}
}

define_serve_with_shutdown!(TokioExec, BoxFuture, Send);
define_serve_with_shutdown!(LocalExec, LocalBoxFuture);
