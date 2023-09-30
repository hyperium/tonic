//! Server implementation and builder.

mod conn;
pub(in crate::transport) mod executor;
mod incoming;
mod recover_error;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
mod tls;
#[cfg(unix)]
mod unix;

pub use super::service::{LocalRoutes, Routes};
use super::{LocalExec, TokioExec};

pub use crate::server::NamedService;
pub use conn::{Connected, TcpConnectInfo};
use hyper::server::conn::AddrStream;
#[cfg(feature = "tls")]
pub use tls::ServerTlsConfig;

#[cfg(feature = "tls")]
pub use conn::TlsConnectInfo;
use tower::limit::ConcurrencyLimit;

#[cfg(feature = "tls")]
use super::service::TlsAcceptor;

#[cfg(unix)]
pub use unix::UdsConnectInfo;

pub use incoming::TcpIncoming;

#[cfg(feature = "tls")]
pub(crate) use tokio_rustls::server::TlsStream;

#[cfg(feature = "tls")]
use crate::transport::Error;

use self::{
    executor::{HasBoxCloneService, HasBoxedCloneService, HttpServiceExecutor, ServeWithShutdown},
    recover_error::RecoverError,
};
use super::service::{GrpcTimeout, ServerIo};
use bytes::Bytes;
use http::{Request, Response};
use hyper::Body;
use pin_project::pin_project;
use std::{
    fmt,
    future::{self, Future},
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{ready, Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_stream::Stream;
use tower::{
    layer::util::{Identity, Stack},
    layer::Layer,
    limit::concurrency::ConcurrencyLimitLayer,
    util::Either,
    Service, ServiceBuilder,
};

type TraceInterceptor = Arc<dyn Fn(&http::Request<()>) -> tracing::Span + Send + Sync + 'static>;

/// A default batteries included `transport` server.
///
/// This is a wrapper around [`hyper::Server`] and provides an easy builder
/// pattern style builder [`Server`]. This builder exposes easy configuration parameters
/// for providing a fully featured http2 based gRPC server. This should provide
/// a very good out of the box http2 server for use with tonic but is also a
/// reference implementation that should be a good starting point for anyone
/// wanting to create a more complex and/or specific implementation.
#[derive(Clone)]
pub struct Server<Ex = TokioExec, L = Identity> {
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
    http2_keepalive_interval: Option<Duration>,
    http2_keepalive_timeout: Option<Duration>,
    http2_adaptive_window: Option<bool>,
    http2_max_pending_accept_reset_streams: Option<usize>,
    max_frame_size: Option<u32>,
    accept_http1: bool,
    service_builder: ServiceBuilder<L>,
    exec: Ex,
}

/// A type alias of [`Server`] for thread-local usage
pub type LocalServer<L = Identity> = Server<LocalExec, L>;

impl<Ex> Default for Server<Ex>
where
    Ex: Default,
{
    fn default() -> Self {
        Self {
            trace_interceptor: None,
            concurrency_limit: None,
            timeout: None,
            #[cfg(feature = "tls")]
            tls: None,
            init_stream_window_size: None,
            init_connection_window_size: None,
            max_concurrent_streams: None,
            tcp_keepalive: None,
            tcp_nodelay: false,
            http2_keepalive_interval: None,
            http2_keepalive_timeout: None,
            http2_adaptive_window: None,
            http2_max_pending_accept_reset_streams: None,
            max_frame_size: None,
            accept_http1: false,
            service_builder: Default::default(),
            exec: Default::default(),
        }
    }
}

/// A stack based `Service` router.
#[derive(Debug)]
pub struct Router<Ex = TokioExec, L = Identity>
where
    Ex: HasBoxCloneService,
{
    server: Server<Ex, L>,
    routes: Routes<Ex>,
}

/// A type alias of [`Router`] for thread-local usage
pub type LocalRouter<L = Identity> = Router<LocalExec, L>;

impl<S: NamedService, T> NamedService for Either<S, T> {
    const NAME: &'static str = S::NAME;
}

impl Server<TokioExec> {
    /// Create a new server builder that can configure a [`Server`].
    pub fn builder() -> Self {
        Server {
            tcp_nodelay: true,
            accept_http1: false,
            exec: TokioExec,
            ..Default::default()
        }
    }
}

impl<L> Server<TokioExec, L> {
    /// Use thread-local executor.
    pub fn local_executor(self) -> Server<LocalExec, L> {
        Server {
            service_builder: self.service_builder,
            trace_interceptor: self.trace_interceptor,
            concurrency_limit: self.concurrency_limit,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            init_stream_window_size: self.init_stream_window_size,
            init_connection_window_size: self.init_connection_window_size,
            max_concurrent_streams: self.max_concurrent_streams,
            tcp_keepalive: self.tcp_keepalive,
            tcp_nodelay: self.tcp_nodelay,
            http2_keepalive_interval: self.http2_keepalive_interval,
            http2_keepalive_timeout: self.http2_keepalive_timeout,
            http2_adaptive_window: self.http2_adaptive_window,
            http2_max_pending_accept_reset_streams: self.http2_max_pending_accept_reset_streams,
            max_frame_size: self.max_frame_size,
            accept_http1: self.accept_http1,
            exec: LocalExec,
        }
    }
}

impl<Ex, L> Server<Ex, L> {
    /// Configure TLS for this server.
    #[cfg(feature = "tls")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
    pub fn tls_config(self, tls_config: ServerTlsConfig) -> Result<Self, Error> {
        Ok(Server {
            tls: Some(tls_config.tls_acceptor().map_err(Error::from_source)?),
            ..self
        })
    }

    /// Set the concurrency limit applied to on requests inbound per connection.
    ///
    /// # Example
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// # let builder = Server::builder();
    /// builder.concurrency_limit_per_connection(32);
    /// ```
    #[must_use]
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
    /// # let builder = Server::builder();
    /// builder.timeout(Duration::from_secs(30));
    /// ```
    #[must_use]
    pub fn timeout(self, timeout: Duration) -> Self {
        Server {
            timeout: Some(timeout),
            ..self
        }
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// Default is 65,535
    ///
    /// [spec]: https://httpwg.org/specs/rfc9113.html#InitialWindowSize
    #[must_use]
    pub fn initial_stream_window_size(self, sz: impl Into<Option<u32>>) -> Self {
        Server {
            init_stream_window_size: sz.into(),
            ..self
        }
    }

    /// Sets the max connection-level flow control for HTTP2
    ///
    /// Default is 65,535
    #[must_use]
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
    /// [spec]: https://httpwg.org/specs/rfc9113.html#n-stream-concurrency
    #[must_use]
    pub fn max_concurrent_streams(self, max: impl Into<Option<u32>>) -> Self {
        Server {
            max_concurrent_streams: max.into(),
            ..self
        }
    }

    /// Set whether HTTP2 Ping frames are enabled on accepted connections.
    ///
    /// If `None` is specified, HTTP2 keepalive is disabled, otherwise the duration
    /// specified will be the time interval between HTTP2 Ping frames.
    /// The timeout for receiving an acknowledgement of the keepalive ping
    /// can be set with [`Server::http2_keepalive_timeout`].
    ///
    /// Default is no HTTP2 keepalive (`None`)
    ///
    #[must_use]
    pub fn http2_keepalive_interval(self, http2_keepalive_interval: Option<Duration>) -> Self {
        Server {
            http2_keepalive_interval,
            ..self
        }
    }

    /// Sets a timeout for receiving an acknowledgement of the keepalive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will be closed.
    /// Does nothing if http2_keep_alive_interval is disabled.
    ///
    /// Default is 20 seconds.
    ///
    #[must_use]
    pub fn http2_keepalive_timeout(self, http2_keepalive_timeout: Option<Duration>) -> Self {
        Server {
            http2_keepalive_timeout,
            ..self
        }
    }

    /// Sets whether to use an adaptive flow control. Defaults to false.
    /// Enabling this will override the limits set in http2_initial_stream_window_size and
    /// http2_initial_connection_window_size.
    #[must_use]
    pub fn http2_adaptive_window(self, enabled: Option<bool>) -> Self {
        Server {
            http2_adaptive_window: enabled,
            ..self
        }
    }

    /// Configures the maximum number of pending reset streams allowed before a GOAWAY will be sent.
    ///
    /// This will default to whatever the default in h2 is. As of v0.3.17, it is 20.
    ///
    /// See <https://github.com/hyperium/hyper/issues/2877> for more information.
    #[must_use]
    pub fn http2_max_pending_accept_reset_streams(self, max: Option<usize>) -> Self {
        Server {
            http2_max_pending_accept_reset_streams: max,
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
    #[must_use]
    pub fn tcp_keepalive(self, tcp_keepalive: Option<Duration>) -> Self {
        Server {
            tcp_keepalive,
            ..self
        }
    }

    /// Set the value of `TCP_NODELAY` option for accepted connections. Enabled by default.
    #[must_use]
    pub fn tcp_nodelay(self, enabled: bool) -> Self {
        Server {
            tcp_nodelay: enabled,
            ..self
        }
    }

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, will default from underlying transport.
    #[must_use]
    pub fn max_frame_size(self, frame_size: impl Into<Option<u32>>) -> Self {
        Server {
            max_frame_size: frame_size.into(),
            ..self
        }
    }

    /// Allow this server to accept http1 requests.
    ///
    /// Accepting http1 requests is only useful when developing `grpc-web`
    /// enabled services. If this setting is set to `true` but services are
    /// not correctly configured to handle grpc-web requests, your server may
    /// return confusing (but correct) protocol errors.
    ///
    /// Default is `false`.
    #[must_use]
    pub fn accept_http1(self, accept_http1: bool) -> Self {
        Server {
            accept_http1,
            ..self
        }
    }

    /// Intercept inbound headers and add a [`tracing::Span`] to each response future.
    #[must_use]
    pub fn trace_fn<F>(self, f: F) -> Self
    where
        F: Fn(&http::Request<()>) -> tracing::Span + Send + Sync + 'static,
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
    pub fn add_service<S>(&mut self, svc: S) -> Router<Ex, L>
    where
        Ex: HasBoxedCloneService<S> + Clone,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
        L: Clone,
    {
        Router::new(self.clone(), Routes::new(svc))
    }

    /// Create a router with the optional `S` typed service as the first service.
    ///
    /// This will clone the `Server` builder and create a router that will
    /// route around different services.
    ///
    /// # Note
    /// Even when the argument given is `None` this will capture *all* requests to this service name.
    /// As a result, one cannot use this to toggle between two identically named implementations.
    pub fn add_optional_service<S>(&mut self, svc: Option<S>) -> Router<Ex, L>
    where
        Ex: HasBoxedCloneService<S> + Clone,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
        L: Clone,
    {
        let routes = if let Some(svc) = svc {
            Routes::new(svc)
        } else {
            Default::default()
        };
        Router::new(self.clone(), routes)
    }

    /// Create a router with given [`Routes`].
    ///
    /// This will clone the `Server` builder and create a router that will
    /// route around different services that were already added to the provided `routes`.
    pub fn add_routes(&mut self, routes: Routes<Ex>) -> Router<Ex, L>
    where
        Ex: HasBoxCloneService + Clone,
        L: Clone,
    {
        Router::new(self.clone(), routes)
    }

    /// Set the [Tower] [`Layer`] all services will be wrapped in.
    ///
    /// This enables using middleware from the [Tower ecosystem][eco].
    ///
    /// # Example
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// use tower::timeout::TimeoutLayer;
    /// use std::time::Duration;
    ///
    /// # let mut builder = Server::builder();
    /// builder.layer(TimeoutLayer::new(Duration::from_secs(30)));
    /// ```
    ///
    /// Note that timeouts should be set using [`Server::timeout`]. `TimeoutLayer` is only used
    /// here as an example.
    ///
    /// You can build more complex layers using [`ServiceBuilder`]. Those layers can include
    /// [interceptors]:
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// use tower::ServiceBuilder;
    /// use std::time::Duration;
    /// use tonic::{Request, Status, service::interceptor};
    ///
    /// fn auth_interceptor(request: Request<()>) -> Result<Request<()>, Status> {
    ///     if valid_credentials(&request) {
    ///         Ok(request)
    ///     } else {
    ///         Err(Status::unauthenticated("invalid credentials"))
    ///     }
    /// }
    ///
    /// fn valid_credentials(request: &Request<()>) -> bool {
    ///     // ...
    ///     # true
    /// }
    ///
    /// fn some_other_interceptor(request: Request<()>) -> Result<Request<()>, Status> {
    ///     Ok(request)
    /// }
    ///
    /// let layer = ServiceBuilder::new()
    ///     .load_shed()
    ///     .timeout(Duration::from_secs(30))
    ///     .layer(interceptor(auth_interceptor))
    ///     .layer(interceptor(some_other_interceptor))
    ///     .into_inner();
    ///
    /// Server::builder().layer(layer);
    /// ```
    ///
    /// [Tower]: https://github.com/tower-rs/tower
    /// [`Layer`]: tower::layer::Layer
    /// [eco]: https://github.com/tower-rs
    /// [`ServiceBuilder`]: tower::ServiceBuilder
    /// [interceptors]: crate::service::Interceptor
    pub fn layer<NewLayer>(self, new_layer: NewLayer) -> Server<Ex, Stack<NewLayer, L>> {
        Server {
            service_builder: self.service_builder.layer(new_layer),
            trace_interceptor: self.trace_interceptor,
            concurrency_limit: self.concurrency_limit,
            timeout: self.timeout,
            #[cfg(feature = "tls")]
            tls: self.tls,
            init_stream_window_size: self.init_stream_window_size,
            init_connection_window_size: self.init_connection_window_size,
            max_concurrent_streams: self.max_concurrent_streams,
            tcp_keepalive: self.tcp_keepalive,
            tcp_nodelay: self.tcp_nodelay,
            http2_keepalive_interval: self.http2_keepalive_interval,
            http2_keepalive_timeout: self.http2_keepalive_timeout,
            http2_adaptive_window: self.http2_adaptive_window,
            http2_max_pending_accept_reset_streams: self.http2_max_pending_accept_reset_streams,
            max_frame_size: self.max_frame_size,
            accept_http1: self.accept_http1,
            exec: self.exec,
        }
    }
}

impl<Ex, L> Router<Ex, L>
where
    Ex: HasBoxCloneService,
{
    pub(crate) fn new(server: Server<Ex, L>, routes: Routes<Ex>) -> Self {
        Self { server, routes }
    }

    /// Add a new service to this router.
    pub fn add_service<S>(mut self, svc: S) -> Self
    where
        Ex: HasBoxedCloneService<S>,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
    {
        self.routes.add_service(svc);
        self
    }

    /// Add a new optional service to this router.
    ///
    /// # Note
    /// Even when the argument given is `None` this will capture *all* requests to this service name.
    /// As a result, one cannot use this to toggle between two identically named implementations.
    #[allow(clippy::type_complexity)]
    pub fn add_optional_service<S>(mut self, svc: Option<S>) -> Self
    where
        Ex: HasBoxedCloneService<S>,
        S: Service<Request<Body>> + NamedService,
        S::Error: Into<crate::Error> + Send,
    {
        if let Some(svc) = svc {
            self.routes.add_service(svc);
        }
        self
    }

    /// Convert this tonic `Router` into a `Service` consuming self.
    pub fn into_router(self) -> Routes<Ex> {
        self.routes
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [tokio]'s default executor.
    ///
    /// [`Server`]: struct.Server.html
    /// [tokio]: https://docs.rs/tokio
    pub async fn serve<ResBody>(self, addr: SocketAddr) -> Result<(), super::Error>
    where
        Ex: ServeWithShutdown<
            L,
            Routes<Ex>,
            TcpIncoming,
            future::Ready<()>,
            AddrStream,
            std::io::Error,
            ResBody,
        >,
        L: Layer<Routes<Ex>> + Send + 'static,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone,
        ResBody: http_body::Body<Data = Bytes>,
    {
        let incoming = TcpIncoming::new(addr, self.server.tcp_nodelay, self.server.tcp_keepalive)
            .map_err(super::Error::from_source)?;
        Ex::serve_with_shutdown(self.server, self.routes, incoming, None).await
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [tokio]'s default executor. And shutdown when the provided signal
    /// is received.
    ///
    /// [`Server`]: struct.Server.html
    /// [tokio]: https://docs.rs/tokio
    pub async fn serve_with_shutdown<F: Future<Output = ()> + 'static, ResBody>(
        self,
        addr: SocketAddr,
        signal: F,
    ) -> Result<(), super::Error>
    where
        Ex: ServeWithShutdown<L, Routes<Ex>, TcpIncoming, F, AddrStream, std::io::Error, ResBody>,
        L: Layer<Routes<Ex>> + Send + 'static,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone,
        F: Send,
        ResBody: http_body::Body<Data = Bytes>,
    {
        let incoming = TcpIncoming::new(addr, self.server.tcp_nodelay, self.server.tcp_keepalive)
            .map_err(super::Error::from_source)?;
        Ex::serve_with_shutdown(self.server, self.routes, incoming, Some(signal)).await
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on the provided incoming stream of `AsyncRead + AsyncWrite`.
    ///
    /// This method discards any provided [`Server`] TCP configuration.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_incoming<I, IO, IE, ResBody>(
        self,
        incoming: I,
    ) -> Result<(), super::Error>
    where
        Ex: ServeWithShutdown<L, Routes<Ex>, I, future::Ready<()>, IO, IE, ResBody>,
        L: Layer<Routes<Ex>> + Send + 'static,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone,
        I: Stream<Item = Result<IO, IE>> + Send + 'static,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IO::ConnectInfo: Clone + Send + Sync + 'static,
        IE: Into<crate::Error> + 'static,
        ResBody: http_body::Body<Data = Bytes>,
    {
        Ex::serve_with_shutdown(self.server, self.routes, incoming, None).await
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on the provided incoming stream of `AsyncRead + AsyncWrite`. Similar to
    /// `serve_with_shutdown` this method will also take a signal future to
    /// gracefully shutdown the server.
    ///
    /// This method discards any provided [`Server`] TCP configuration.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_incoming_shutdown<I, IO, IE, F, ResBody>(
        self,
        incoming: I,
        signal: F,
    ) -> Result<(), super::Error>
    where
        Ex: ServeWithShutdown<L, Routes<Ex>, I, F, IO, IE, ResBody>,
        L: Layer<Routes<Ex>> + Send + 'static,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone,
        I: Stream<Item = Result<IO, IE>> + Send + 'static,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IO::ConnectInfo: Clone + Send + Sync + 'static,
        IE: Into<crate::Error> + 'static,
        F: Future<Output = ()> + Send + 'static,
        ResBody: http_body::Body<Data = Bytes>,
    {
        Ex::serve_with_shutdown(self.server, self.routes, incoming, Some(signal)).await
    }

    /// Create a tower service out of a router.
    pub fn into_service<ResBody>(self) -> L::Service
    where
        Ex: HttpServiceExecutor<L::Service, ResBody>,
        L: Layer<Routes<Ex>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>>,
    {
        self.server.service_builder.service(self.routes)
    }
}

impl<Ex, L> fmt::Debug for Server<Ex, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder").finish()
    }
}

#[allow(missing_docs, missing_debug_implementations)]
pub struct Svc<Ex, S, C> {
    inner: S,
    #[cfg(feature = "tls")]
    conn_info: Either<C, TlsConnectInfo<C>>,
    #[cfg(not(feature = "tls"))]
    conn_info: Either<C, ()>,
    trace_interceptor: Option<TraceInterceptor>,
    _marker: PhantomData<Ex>,
}

impl<Ex, S, C, ResBody> Service<Request<Body>> for Svc<Ex, S, C>
where
    Ex: HttpServiceExecutor<S, ResBody>,
    S: Service<Request<Body>, Response = Response<ResBody>>,
    S::Error: Into<crate::Error>,
    C: Clone + Sync + Send + 'static,
{
    type Response = Response<Ex::BoxHttpBody>;
    type Error = crate::Error;
    type Future = SvcFuture<Ex, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        match &self.conn_info {
            tower::util::Either::A(inner) => {
                req.extensions_mut().insert(inner.clone());
            }
            tower::util::Either::B(inner) => {
                #[cfg(feature = "tls")]
                {
                    req.extensions_mut().insert(inner.clone());
                    req.extensions_mut().insert(inner.get_ref().clone());
                }
                #[cfg(not(feature = "tls"))]
                {
                    // just a type check to make sure we didn't forget to
                    // insert this into the extensions
                    let _: &() = inner;
                }
            }
        }

        let span = if let Some(trace_interceptor) = &self.trace_interceptor {
            let (parts, body) = req.into_parts();
            let bodyless_request = Request::from_parts(parts, ());

            let span = trace_interceptor(&bodyless_request);

            let (parts, _) = bodyless_request.into_parts();
            req = Request::from_parts(parts, body);

            span
        } else {
            tracing::Span::none()
        };

        SvcFuture {
            inner: self.inner.call(req),
            span,
            _marker: PhantomData::<Ex>,
        }
    }
}

#[allow(missing_docs, missing_debug_implementations)]
#[pin_project]
pub struct SvcFuture<Ex, F> {
    #[pin]
    inner: F,
    span: tracing::Span,
    _marker: PhantomData<Ex>,
}

impl<Ex, F, E, ResBody> Future for SvcFuture<Ex, F>
where
    Ex: executor::HasBoxedHttpBody<F, ResBody>,
    F: Future<Output = Result<Response<ResBody>, E>>,
    E: Into<crate::Error>,
{
    type Output = Result<Response<Ex::BoxHttpBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _guard = this.span.enter();

        let response: Response<ResBody> = ready!(this.inner.poll(cx)).map_err(Into::into)?;
        let response = response.map(Ex::boxed_http_body);
        Poll::Ready(Ok(response))
    }
}

impl<Ex, S, C> fmt::Debug for Svc<Ex, S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Svc").finish()
    }
}

#[allow(missing_docs, missing_debug_implementations)]
pub struct MakeSvc<Ex, S, IO> {
    concurrency_limit: Option<usize>,
    timeout: Option<Duration>,
    inner: S,
    trace_interceptor: Option<TraceInterceptor>,
    _marker: PhantomData<(Ex, IO)>,
}

impl<Ex, S, ResBody, IO> Service<&ServerIo<IO>> for MakeSvc<Ex, S, IO>
where
    Ex: HttpServiceExecutor<S, ResBody>,
    S: Service<Request<Body>, Response = Response<ResBody>> + Clone,
    ResBody: http_body::Body<Data = Bytes>,
    IO: Connected,
{
    type Response = Svc<
        Ex,
        RecoverError<Either<ConcurrencyLimit<GrpcTimeout<S>>, GrpcTimeout<S>>>,
        IO::ConnectInfo,
    >;
    type Error = crate::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, io: &ServerIo<IO>) -> Self::Future {
        let conn_info = io.connect_info();

        let svc = self.inner.clone();
        let concurrency_limit = self.concurrency_limit;
        let timeout = self.timeout;
        let trace_interceptor = self.trace_interceptor.clone();

        let svc = ServiceBuilder::new()
            .layer_fn(RecoverError::new)
            .option_layer(concurrency_limit.map(ConcurrencyLimitLayer::new))
            .layer_fn(|s| GrpcTimeout::new(s, timeout))
            .service(svc);

        future::ready(Ok(Svc {
            inner: svc,
            conn_info,
            trace_interceptor,
            _marker: PhantomData::<Ex>,
        }))
    }
}
