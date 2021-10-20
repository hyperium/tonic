//! Server implementation and builder.

mod conn;
mod incoming;
mod recover_error;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
mod tls;

pub use conn::{Connected, TcpConnectInfo};
#[cfg(feature = "tls")]
pub use tls::ServerTlsConfig;

#[cfg(feature = "tls")]
pub use conn::TlsConnectInfo;

#[cfg(feature = "tls")]
use super::service::TlsAcceptor;

use incoming::TcpIncoming;

#[cfg(feature = "tls")]
pub(crate) use tokio_rustls::server::TlsStream;

#[cfg(feature = "tls")]
use crate::transport::Error;

use self::recover_error::RecoverError;
use super::service::{GrpcTimeout, Or, Routes, ServerIo};
use crate::body::BoxBody;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::{
    future::{self, MapErr},
    ready, TryFutureExt,
};
use http::{Request, Response};
use http_body::Body as _;
use hyper::{server::accept, Body};
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tower::{
    layer::util::Identity, layer::Layer, limit::concurrency::ConcurrencyLimitLayer, util::Either,
    Service, ServiceBuilder,
};

type BoxHttpBody = http_body::combinators::BoxBody<Bytes, crate::Error>;
type BoxService = tower::util::BoxService<Request<Body>, Response<BoxHttpBody>, crate::Error>;
type TraceInterceptor = Arc<dyn Fn(&http::Request<()>) -> tracing::Span + Send + Sync + 'static>;

const DEFAULT_HTTP2_KEEPALIVE_TIMEOUT_SECS: u64 = 20;

/// A default batteries included `transport` server.
///
/// This is a wrapper around [`hyper::Server`] and provides an easy builder
/// pattern style builder [`Server`]. This builder exposes easy configuration parameters
/// for providing a fully featured http2 based gRPC server. This should provide
/// a very good out of the box http2 server for use with tonic but is also a
/// reference implementation that should be a good starting point for anyone
/// wanting to create a more complex and/or specific implementation.
#[derive(Default, Clone)]
pub struct Server<L = Identity> {
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
    max_frame_size: Option<u32>,
    accept_http1: bool,
    layer: L,
}

/// A stack based `Service` router.
#[derive(Debug)]
pub struct Router<A, B, L = Identity> {
    server: Server<L>,
    routes: Routes<A, B, Request<Body>>,
}

/// A service that is produced from a Tonic `Router`.
///
/// This service implementation will route between multiple Tonic
/// gRPC endpoints and can be consumed with the rest of the `tower`
/// ecosystem.
#[derive(Debug, Clone)]
pub struct RouterService<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for RouterService<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;

    #[allow(clippy::type_complexity)]
    type Future = MapErr<S::Future, fn(S::Error) -> crate::Error>;

    #[inline]
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.inner.call(req).map_err(Into::into)
    }
}

/// A trait to provide a static reference to the service's
/// name. This is used for routing service's within the router.
pub trait NamedService {
    /// The `Service-Name` as described [here].
    ///
    /// [here]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
    const NAME: &'static str;
}

impl<S: NamedService, T> NamedService for Either<S, T> {
    const NAME: &'static str = S::NAME;
}

impl Server {
    /// Create a new server builder that can configure a [`Server`].
    pub fn builder() -> Self {
        Server {
            tcp_nodelay: true,
            accept_http1: false,
            ..Default::default()
        }
    }
}

impl<L> Server<L> {
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

    /// Set whether HTTP2 Ping frames are enabled on accepted connections.
    ///
    /// If `None` is specified, HTTP2 keepalive is disabled, otherwise the duration
    /// specified will be the time interval between HTTP2 Ping frames.
    /// The timeout for receiving an acknowledgement of the keepalive ping
    /// can be set with [`Server::http2_keepalive_timeout`].
    ///
    /// Default is no HTTP2 keepalive (`None`)
    ///
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
    pub fn http2_keepalive_timeout(self, http2_keepalive_timeout: Option<Duration>) -> Self {
        Server {
            http2_keepalive_timeout,
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

    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, will default from underlying transport.
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
    pub fn accept_http1(self, accept_http1: bool) -> Self {
        Server {
            accept_http1,
            ..self
        }
    }

    /// Intercept inbound headers and add a [`tracing::Span`] to each response future.
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
    pub fn add_service<S>(&mut self, svc: S) -> Router<S, Unimplemented, L>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
        L: Clone,
    {
        Router::new(self.clone(), svc)
    }

    /// Create a router with the optional `S` typed service as the first service.
    ///
    /// This will clone the `Server` builder and create a router that will
    /// route around different services.
    ///
    /// # Note
    /// Even when the argument given is `None` this will capture *all* requests to this service name.
    /// As a result, one cannot use this to toggle between two identically named implementations.
    pub fn add_optional_service<S>(
        &mut self,
        svc: Option<S>,
    ) -> Router<Either<S, Unimplemented>, Unimplemented, L>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
        L: Clone,
    {
        let svc = match svc {
            Some(some) => Either::A(some),
            None => Either::B(Unimplemented::default()),
        };
        Router::new(self.clone(), svc)
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
    pub fn layer<NewLayer>(self, new_layer: NewLayer) -> Server<NewLayer> {
        Server {
            layer: new_layer,
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
            max_frame_size: self.max_frame_size,
            accept_http1: self.accept_http1,
        }
    }

    pub(crate) async fn serve_with_shutdown<S, I, F, IO, IE, ResBody>(
        self,
        svc: S,
        incoming: I,
        signal: Option<F>,
    ) -> Result<(), super::Error>
    where
        L: Layer<S>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<S>>::Service as Service<Request<Body>>>::Future: Send + 'static,
        <<L as Layer<S>>::Service as Service<Request<Body>>>::Error: Into<crate::Error> + Send,
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IO::ConnectInfo: Clone + Send + Sync + 'static,
        IE: Into<crate::Error>,
        F: Future<Output = ()>,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
        let trace_interceptor = self.trace_interceptor.clone();
        let concurrency_limit = self.concurrency_limit;
        let init_connection_window_size = self.init_connection_window_size;
        let init_stream_window_size = self.init_stream_window_size;
        let max_concurrent_streams = self.max_concurrent_streams;
        let timeout = self.timeout;
        let max_frame_size = self.max_frame_size;
        let http2_only = !self.accept_http1;

        let http2_keepalive_interval = self.http2_keepalive_interval;
        let http2_keepalive_timeout = self
            .http2_keepalive_timeout
            .unwrap_or_else(|| Duration::new(DEFAULT_HTTP2_KEEPALIVE_TIMEOUT_SECS, 0));

        let svc = self.layer.layer(svc);

        let tcp = incoming::tcp_incoming(incoming, self);
        let incoming = accept::from_stream::<_, _, crate::Error>(tcp);

        let svc = MakeSvc {
            inner: svc,
            concurrency_limit,
            timeout,
            trace_interceptor,
            _io: PhantomData,
        };

        let server = hyper::Server::builder(incoming)
            .http2_only(http2_only)
            .http2_initial_connection_window_size(init_connection_window_size)
            .http2_initial_stream_window_size(init_stream_window_size)
            .http2_max_concurrent_streams(max_concurrent_streams)
            .http2_keep_alive_interval(http2_keepalive_interval)
            .http2_keep_alive_timeout(http2_keepalive_timeout)
            .http2_max_frame_size(max_frame_size);

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

impl<S, L> Router<S, Unimplemented, L> {
    pub(crate) fn new(server: Server<L>, svc: S) -> Self
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

impl<A, B, L> Router<A, B, L>
where
    A: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    A::Future: Send + 'static,
    A::Error: Into<crate::Error> + Send,
    B: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
    B::Future: Send + 'static,
    B::Error: Into<crate::Error> + Send,
{
    /// Add a new service to this router.
    pub fn add_service<S>(self, svc: S) -> Router<S, Or<A, B, Request<Body>>, L>
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

    /// Add a new optional service to this router.
    ///
    /// # Note
    /// Even when the argument given is `None` this will capture *all* requests to this service name.
    /// As a result, one cannot use this to toggle between two identically named implementations.
    #[allow(clippy::type_complexity)]
    pub fn add_optional_service<S>(
        self,
        svc: Option<S>,
    ) -> Router<Either<S, Unimplemented>, Or<A, B, Request<Body>>, L>
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
        let svc = match svc {
            Some(some) => Either::A(some),
            None => Either::B(Unimplemented::default()),
        };
        let routes = routes.push(pred, svc);

        Router { server, routes }
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [tokio]'s default executor.
    ///
    /// [`Server`]: struct.Server.html
    /// [tokio]: https://docs.rs/tokio
    pub async fn serve<ResBody>(self, addr: SocketAddr) -> Result<(), super::Error>
    where
        L: Layer<Routes<A, B, Request<Body>>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Future:
            Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Error:
            Into<crate::Error> + Send,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
        let incoming = TcpIncoming::new(addr, self.server.tcp_nodelay, self.server.tcp_keepalive)
            .map_err(super::Error::from_source)?;
        self.server
            .serve_with_shutdown::<_, _, future::Ready<()>, _, _, ResBody>(
                self.routes,
                incoming,
                None,
            )
            .await
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [tokio]'s default executor. And shutdown when the provided signal
    /// is received.
    ///
    /// [`Server`]: struct.Server.html
    /// [tokio]: https://docs.rs/tokio
    pub async fn serve_with_shutdown<F: Future<Output = ()>, ResBody>(
        self,
        addr: SocketAddr,
        signal: F,
    ) -> Result<(), super::Error>
    where
        L: Layer<Routes<A, B, Request<Body>>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Future:
            Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Error:
            Into<crate::Error> + Send,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
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
    pub async fn serve_with_incoming<I, IO, IE, ResBody>(
        self,
        incoming: I,
    ) -> Result<(), super::Error>
    where
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IO::ConnectInfo: Clone + Send + Sync + 'static,
        IE: Into<crate::Error>,
        L: Layer<Routes<A, B, Request<Body>>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Future:
            Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Error:
            Into<crate::Error> + Send,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
        self.server
            .serve_with_shutdown::<_, _, future::Ready<()>, _, _, ResBody>(
                self.routes,
                incoming,
                None,
            )
            .await
    }

    /// Consume this [`Server`] creating a future that will execute the server on
    /// the provided incoming stream of `AsyncRead + AsyncWrite`. Similar to
    /// `serve_with_shutdown` this method will also take a signal future to
    /// gracefully shutdown the server.
    ///
    /// [`Server`]: struct.Server.html
    pub async fn serve_with_incoming_shutdown<I, IO, IE, F, ResBody>(
        self,
        incoming: I,
        signal: F,
    ) -> Result<(), super::Error>
    where
        I: Stream<Item = Result<IO, IE>>,
        IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
        IO::ConnectInfo: Clone + Send + Sync + 'static,
        IE: Into<crate::Error>,
        F: Future<Output = ()>,
        L: Layer<Routes<A, B, Request<Body>>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Future:
            Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Error:
            Into<crate::Error> + Send,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
        self.server
            .serve_with_shutdown(self.routes, incoming, Some(signal))
            .await
    }

    /// Create a tower service out of a router.
    pub fn into_service<ResBody>(self) -> RouterService<L::Service>
    where
        L: Layer<Routes<A, B, Request<Body>>>,
        L::Service: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Future:
            Send + 'static,
        <<L as Layer<Routes<A, B, Request<Body>>>>::Service as Service<Request<Body>>>::Error:
            Into<crate::Error> + Send,
        ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        ResBody::Error: Into<crate::Error>,
    {
        let inner = self.server.layer.layer(self.routes);
        RouterService { inner }
    }
}

impl<L> fmt::Debug for Server<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder").finish()
    }
}

struct Svc<S> {
    inner: S,
    trace_interceptor: Option<TraceInterceptor>,
}

impl<S, ResBody> Service<Request<Body>> for Svc<S>
where
    S: Service<Request<Body>, Response = Response<ResBody>>,
    S::Error: Into<crate::Error>,
    ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Response = Response<BoxHttpBody>;
    type Error = crate::Error;
    type Future = SvcFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
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
        }
    }
}

#[pin_project]
struct SvcFuture<F> {
    #[pin]
    inner: F,
    span: tracing::Span,
}

impl<F, E, ResBody> Future for SvcFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
    E: Into<crate::Error>,
    ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Output = Result<Response<BoxHttpBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _guard = this.span.enter();

        let response: Response<ResBody> = ready!(this.inner.poll(cx)).map_err(Into::into)?;
        let response = response.map(|body| body.map_err(Into::into).boxed());
        Poll::Ready(Ok(response))
    }
}

impl<S> fmt::Debug for Svc<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Svc").finish()
    }
}

struct MakeSvc<S, IO> {
    concurrency_limit: Option<usize>,
    timeout: Option<Duration>,
    inner: S,
    trace_interceptor: Option<TraceInterceptor>,
    _io: PhantomData<fn() -> IO>,
}

impl<S, ResBody, IO> Service<&ServerIo<IO>> for MakeSvc<S, IO>
where
    IO: Connected,
    S: Service<Request<Body>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
    ResBody: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Response = BoxService;
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

        let svc = ServiceBuilder::new()
            .layer(BoxService::layer())
            .map_request(move |mut request: Request<Body>| {
                match &conn_info {
                    tower::util::Either::A(inner) => {
                        request.extensions_mut().insert(inner.clone());
                    }
                    tower::util::Either::B(inner) => {
                        #[cfg(feature = "tls")]
                        {
                            request.extensions_mut().insert(inner.clone());
                            request.extensions_mut().insert(inner.get_ref().clone());
                        }

                        #[cfg(not(feature = "tls"))]
                        {
                            // just a type check to make sure we didn't forget to
                            // insert this into the extensions
                            let _: &() = inner;
                        }
                    }
                }

                request
            })
            .service(Svc {
                inner: svc,
                trace_interceptor,
            });

        future::ready(Ok(svc))
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
                .header("content-type", "application/grpc")
                .body(crate::body::empty_body())
                .unwrap(),
        )
    }
}
