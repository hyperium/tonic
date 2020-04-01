//! Server implementation and builder.

mod conn;
mod incoming;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
mod tls;

pub use conn::Connected;
#[cfg(feature = "tls")]
pub use tls::ServerTlsConfig;

#[cfg(feature = "tls")]
use super::service::TlsAcceptor;

use incoming::TcpIncoming;

#[cfg(feature = "tls")]
pub(crate) use incoming::TlsStream;

use super::service::{Or, Routes, ServerIo, ServiceBuilderExt};
use crate::{body::BoxBody, request::ConnectionInfo};
use futures_core::Stream;
use futures_util::{
    future::{self, MapErr},
    TryFutureExt,
};
use http::{HeaderMap, Request, Response};
use hyper::{server::accept, Body};
use std::{
    fmt,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tower::{
    limit::concurrency::ConcurrencyLimitLayer, timeout::TimeoutLayer, Service, ServiceBuilder,
};
use tracing_futures::{Instrument, Instrumented};

type BoxService = tower::util::BoxService<Request<Body>, Response<BoxBody>, crate::Error>;
type TraceInterceptor = Arc<dyn Fn(&HeaderMap) -> tracing::Span + Send + Sync + 'static>;

/// A default batteries included `transport` server.
///
/// This is a wrapper around [`hyper::Server`] and provides an easy builder
/// pattern style builder [`Server`]. This builder exposes easy configuration parameters
/// for providing a fully featured http2 based gRPC server. This should provide
/// a very good out of the box http2 server for use with tonic but is also a
/// reference implementation that should be a good starting point for anyone
/// wanting to create a more complex and/or specific implementation.
#[derive(Default, Clone)]
pub struct Server {
    trace_interceptor: Option<TraceInterceptor>,
    concurrency_limit: Option<usize>,
    timeout: Option<Duration>,
    #[cfg(feature = "tls")]
    tls: Option<TlsAcceptor>,
    init_stream_window_size: Option<u32>,
    init_connection_window_size: Option<u32>,
    max_concurrent_streams: Option<u32>,
    tcp_keepalive: Option<Duration>,
    tcp_nodelay: bool,
}

/// A stack based `Service` router.
#[derive(Debug)]
pub struct Router<A, B> {
    server: Server,
    routes: Routes<A, B, Request<Body>>,
}

/// A trait to provide a static reference to the service's
/// name. This is used for routing service's within the router.
pub trait NamedService {
    /// The `Service-Name` as described [here].
    ///
    /// [here]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
    const NAME: &'static str;
}

impl Server {
    /// Create a new server builder that can configure a [`Server`].
    pub fn builder() -> Self {
        Server {
            tcp_nodelay: true,
            ..Default::default()
        }
    }
}

impl Server {
    /// Configure TLS for this server.
    #[cfg(feature = "tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
    pub fn tls_config(self, tls_config: ServerTlsConfig) -> Self {
        Server {
            tls: Some(tls_config.tls_acceptor().unwrap()),
            ..self
        }
    }

    /// Set the concurrency limit applied to on requests inbound per connection.
    ///
    /// # Example
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// # let mut builder = Server::builder();
    /// builder.concurrency_limit_per_connection(32);
    /// ```
    pub fn concurrency_limit_per_connection(self, limit: usize) -> Self {
        Server {
            concurrency_limit: Some(limit),
            ..self
        }
    }

    /// Set a timeout on for all request handlers.
    ///
    /// # Example
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// # use std::time::Duration;
    /// # let mut builder = Server::builder();
    /// builder.timeout(Duration::from_secs(30));
    /// ```
    pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// Default is 65,535
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_INITIAL_WINDOW_SIZE
    pub fn initial_stream_window_size(self, sz: impl Into<Option<u32>>) -> Self {
        Server {
            init_stream_window_size: sz.into(),
            ..self
        }
    }

    /// Sets the max connection-level flow control for HTTP2
    ///
    /// Default is 65,535
    pub fn initial_connection_window_size(self, sz: impl Into<Option<u32>>) -> Self {
        Server {
            init_connection_window_size: sz.into(),
            ..self
        }
    }

    /// Sets the [`SETTINGS_MAX_CONCURRENT_STREAMS`][spec] option for HTTP2
    /// connections.
    ///
    /// Default is no limit (`None`).
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_MAX_CONCURRENT_STREAMS
    pub fn max_concurrent_streams(self, max: impl Into<Option<u32>>) -> Self {
        Server {
            max_concurrent_streams: max.into(),
            ..self
        }
    }

    /// Set whether TCP keepalive messages are enabled on accepted connections.
    ///
    /// If `None` is specified, keepalive is disabled, otherwise the duration
    /// specified will be the time to remain idle before sending TCP keepalive
    /// probes.
    ///
    /// Default is no keepalive (`None`)
    ///
    pub fn tcp_keepalive(self, tcp_keepalive: Option<Duration>) -> Self {
        Server {
            tcp_keepalive,
            ..self
        }
    }

    /// Set the value of `TCP_NODELAY` option for accepted connections. Enabled by default.
    pub fn tcp_nodelay(self, enabled: bool) -> Self {
        Server {
            tcp_nodelay: enabled,
            ..self
        }
    }

    /// Intercept inbound headers and add a [`tracing::Span`] to each response future.
    pub fn trace_fn<F>(self, f: F) -> Self
    where
        F: Fn(&HeaderMap) -> tracing::Span + Send + Sync + 'static,
    {
        Server {
            trace_interceptor: Some(Arc::new(f)),
            ..self
        }
    }

    /// Create a router with the `S` typed service as the first service.
    ///
    /// This will clone the `Server` builder and create a router that will
    /// route around different services.
    pub fn add_service<S>(&mut self, svc: S) -> Router<S, Unimplemented>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        Router::new(self.clone(), svc)
    }

    pub(crate) async fn serve_with_shutdown<S, I, F, IO, IE>(
        self,
        svc: S,
        incoming: I,
        signal: Option<F>,
    ) -> Result<(), super::Error>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IE: Into<crate::Error>,
        F: Future<Output = ()>,
    {
        let span = self.trace_interceptor.clone();
        let concurrency_limit = self.concurrency_limit;
        let init_connection_window_size = self.init_connection_window_size;
        let init_stream_window_size = self.init_stream_window_size;
        let max_concurrent_streams = self.max_concurrent_streams;
        let timeout = self.timeout.clone();

        let tcp = incoming::tcp_incoming(incoming, self);
        let incoming = accept::from_stream::<_, _, crate::Error>(tcp);

        let svc = MakeSvc {
            inner: svc,
            concurrency_limit,
            timeout,
            span,
        };

        let server = hyper::Server::builder(incoming)
            .http2_only(true)
            .http2_initial_connection_window_size(init_connection_window_size)
            .http2_initial_stream_window_size(init_stream_window_size)
            .http2_max_concurrent_streams(max_concurrent_streams);

        if let Some(signal) = signal {
            server
                .serve(svc)
                .with_graceful_shutdown(signal)
                .await
                .map_err(super::Error::from_source)?
        } else {
            server.serve(svc).await.map_err(super::Error::from_source)?;
        }

        Ok(())
    }
}

impl<S> Router<S, Unimplemented> {
    pub(crate) fn new(server: Server, svc: S) -> Self
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let svc_name = <S as NamedService>::NAME;
        let svc_route = format!("/{}", svc_name);
        let pred = move |req: &Request<Body>| {
            let path = req.uri().path();

            path.starts_with(&svc_route)
        };
        Self {
            server,
            routes: Routes::new(pred, svc, Unimplemented::default()),
        }
    }
}

impl<A, B> Router<A, B>
where
    A: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    A::Future: Send + 'static,
    A::Error: Into<crate::Error> + Send,
    B: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    B::Future: Send + 'static,
    B::Error: Into<crate::Error> + Send,
{
    /// Add a new service to this router.
    pub fn add_service<S>(self, svc: S) -> Router<S, Or<A, B, Request<Body>>>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let Self { routes, server } = self;

        let svc_name = <S as NamedService>::NAME;
        let svc_route = format!("/{}", svc_name);
        let pred = move |req: &Request<Body>| {
            let path = req.uri().path();

            path.starts_with(&svc_route)
        };
        let routes = routes.push(pred, svc);

        Router { server, routes }
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [`tokio`]'s default executor.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve(self, addr: SocketAddr) -> Result<(), super::Error> {
        let incoming = TcpIncoming::new(addr, self.server.tcp_nodelay, self.server.tcp_keepalive)
            .map_err(super::Error::from_source)?;
        self.server
            .serve_with_shutdown::<_, _, future::Ready<()>, _, _>(self.routes, incoming, None)
            .await
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [`tokio`]'s default executor. And shutdown when the provided signal
    /// is received.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_shutdown<F: Future<Output = ()>>(
        self,
        addr: SocketAddr,
        signal: F,
    ) -> Result<(), super::Error> {
        let incoming = TcpIncoming::new(addr, self.server.tcp_nodelay, self.server.tcp_keepalive)
            .map_err(super::Error::from_source)?;
        self.server
            .serve_with_shutdown(self.routes, incoming, Some(signal))
            .await
    }

    /// Consume this [`Server`] creating a future that will execute the server on
    /// the provided incoming stream of `AsyncRead + AsyncWrite`.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_incoming<I, IO, IE>(self, incoming: I) -> Result<(), super::Error>
    where
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IE: Into<crate::Error>,
    {
        self.server
            .serve_with_shutdown::<_, _, future::Ready<()>, _, _>(self.routes, incoming, None)
            .await
    }

    /// Consume this [`Server`] creating a future that will execute the server on
    /// the provided incoming stream of `AsyncRead + AsyncWrite`. Similar to
    /// `serve_with_shutdown` this method will also take a signal future to
    /// gracefully shutdown the server.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_incoming_shutdown<I, IO, IE, F>(
        self,
        incoming: I,
        signal: F,
    ) -> Result<(), super::Error>
    where
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IE: Into<crate::Error>,
        F: Future<Output = ()>,
    {
        self.server
            .serve_with_shutdown(self.routes, incoming, Some(signal))
            .await
    }
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder").finish()
    }
}

struct Svc<S> {
    inner: S,
    span: Option<TraceInterceptor>,
    conn_info: ConnectionInfo,
}

impl<S> Service<Request<Body>> for Svc<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = MapErr<Instrumented<S::Future>, fn(S::Error) -> crate::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let span = if let Some(trace_interceptor) = &self.span {
            trace_interceptor(req.headers())
        } else {
            tracing::Span::none()
        };

        req.extensions_mut().insert(self.conn_info.clone());

        self.inner.call(req).instrument(span).map_err(|e| e.into())
    }
}

impl<S> fmt::Debug for Svc<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Svc").finish()
    }
}

struct MakeSvc<S> {
    concurrency_limit: Option<usize>,
    timeout: Option<Duration>,
    inner: S,
    span: Option<TraceInterceptor>,
}

impl<S> Service<&ServerIo> for MakeSvc<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
{
    type Response = BoxService;
    type Error = crate::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, io: &ServerIo) -> Self::Future {
        let conn_info = crate::request::ConnectionInfo {
            remote_addr: io.remote_addr(),
            peer_certs: io.peer_certs().map(Arc::new),
        };

        let svc = self.inner.clone();
        let concurrency_limit = self.concurrency_limit;
        let timeout = self.timeout.clone();
        let span = self.span.clone();

        Box::pin(async move {
            let svc = ServiceBuilder::new()
                .optional_layer(concurrency_limit.map(ConcurrencyLimitLayer::new))
                .optional_layer(timeout.map(TimeoutLayer::new))
                .service(svc);

            let svc = BoxService::new(Svc {
                inner: svc,
                span,
                conn_info,
            });

            Ok(svc)
        })
    }
}

#[derive(Default, Clone, Debug)]
#[doc(hidden)]
pub struct Unimplemented {
    _p: (),
}

impl Service<Request<Body>> for Unimplemented {
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _req: Request<Body>) -> Self::Future {
        future::ok(
            http::Response::builder()
                .status(200)
                .header("grpc-status", "12")
                .body(BoxBody::empty())
                .unwrap(),
        )
    }
}
