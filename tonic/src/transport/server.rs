//! Server implementation and builder.

use super::service::{layer_fn, BoxedIo, Or, Routes, ServiceBuilderExt};
#[cfg(feature = "tls")]
use super::{service::TlsAcceptor, tls::Identity, Certificate};
use crate::body::BoxBody;
use futures_core::Stream;
use futures_util::{
    future::{self, MapErr},
    ready, TryFutureExt, TryStreamExt,
};
use http::{Request, Response};
use hyper::{
    server::{accept::Accept, conn},
    Body,
};
use std::{
    fmt,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    // time::Duration,
};
use tower::{
    layer::{Layer, Stack},
    limit::concurrency::ConcurrencyLimitLayer,
    // timeout::TimeoutLayer,
    Service,
    ServiceBuilder,
};
#[cfg(feature = "tls")]
use tracing::error;

type BoxService = tower::util::BoxService<Request<Body>, Response<BoxBody>, crate::Error>;
type Interceptor = Arc<dyn Layer<BoxService, Service = BoxService> + Send + Sync + 'static>;

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
    interceptor: Option<Interceptor>,
    concurrency_limit: Option<usize>,
    // timeout: Option<Duration>,
    #[cfg(feature = "tls")]
    tls: Option<TlsAcceptor>,
    init_stream_window_size: Option<u32>,
    init_connection_window_size: Option<u32>,
    max_concurrent_streams: Option<u32>,
}

/// A stack based `Service` router.
#[derive(Debug)]
pub struct Router<A, B> {
    server: Server,
    routes: Routes<A, B, Request<Body>>,
}

/// A trait to provide a static reference to the service's
/// name. This is used for routing service's within the router.
pub trait ServiceName {
    /// The `Service-Name` as described [here].
    ///
    /// [here]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
    const NAME: &'static str;
}

impl Server {
    /// Create a new server builder that can configure a [`Server`].
    pub fn builder() -> Self {
        Default::default()
    }
}

impl Server {
    /// Configure TLS for this server.
    #[cfg(feature = "tls")]
    pub fn tls_config(self, tls_config: ServerTlsConfig) -> Self {
        Server {
            tls: Some(tls_config.tls_acceptor().unwrap()),
            ..self
        }
    }

    /// Set the concurrency limit applied to on requests inbound per connection.
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

    // FIXME: tower-timeout currentlly uses `From` instead of `Into` for the error
    // so our services do not align.
    // pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
    //     self.timeout = Some(timeout);
    //     self
    // }

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

    /// Intercept the execution of gRPC methods.
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// # let mut builder = Server::builder();
    /// builder.interceptor_fn(|svc, req| {
    ///     println!("request={:?}", req);
    ///     svc.call(req)
    /// });
    /// ```
    pub fn interceptor_fn<F, Out>(self, f: F) -> Self
    where
        F: Fn(&mut BoxService, Request<Body>) -> Out + Send + Sync + 'static,
        Out: Future<Output = Result<Response<BoxBody>, crate::Error>> + Send + 'static,
    {
        let f = Arc::new(f);
        let interceptor = layer_fn(move |mut s| {
            let f = f.clone();
            tower::service_fn(move |req| f(&mut s, req))
        });
        let layer = Stack::new(interceptor, layer_fn(BoxService::new));

        Server {
            interceptor: Some(Arc::new(layer)),
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
            + ServiceName
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        Router::new(self.clone(), svc)
    }

    pub(crate) async fn serve<S>(self, addr: SocketAddr, svc: S) -> Result<(), super::Error>
    where
        S: Service<Request<Body>, Response = Response<BoxBody>> + Clone + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let interceptor = self.interceptor.clone();
        let concurrency_limit = self.concurrency_limit;
        let init_connection_window_size = self.init_connection_window_size;
        let init_stream_window_size = self.init_stream_window_size;
        let max_concurrent_streams = self.max_concurrent_streams;
        // let timeout = self.timeout.clone();

        let incoming = hyper::server::accept::from_stream::<_, _, crate::Error>(
            async_stream::try_stream! {
                let mut tcp = TcpIncoming::bind(addr)?;

                while let Some(stream) = tcp.try_next().await? {
                    #[cfg(feature = "tls")]
                    {
                        if let Some(tls) = &self.tls {
                            let io = match tls.connect(stream.into_inner()).await {
                                Ok(io) => io,
                                Err(error) => {
                                    error!(message = "Unable to accept incoming connection.", %error);
                                    continue
                                },
                            };
                            yield BoxedIo::new(io);
                            continue;
                        }
                    }

                    yield BoxedIo::new(stream);
                }
            },
        );

        let svc = MakeSvc {
            inner: svc,
            interceptor,
            concurrency_limit,
            // timeout,
        };

        hyper::Server::builder(incoming)
            .http2_only(true)
            .http2_initial_connection_window_size(init_connection_window_size)
            .http2_initial_stream_window_size(init_stream_window_size)
            .http2_max_concurrent_streams(max_concurrent_streams)
            .serve(svc)
            .await
            .map_err(map_err)?;

        Ok(())
    }
}

impl<S> Router<S, Unimplemented> {
    pub(crate) fn new(server: Server, svc: S) -> Self
    where
        S: Service<Request<Body>, Response = Response<BoxBody>>
            + ServiceName
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let svc_name = <S as ServiceName>::NAME;
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
            + ServiceName
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let Self { routes, server } = self;

        let svc_name = <S as ServiceName>::NAME;
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
        self.server.serve(addr, self.routes).await
    }
}

fn map_err(e: impl Into<crate::Error>) -> super::Error {
    super::Error::from_source(super::ErrorKind::Server, e.into())
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder").finish()
    }
}

/// Configures TLS settings for servers.
#[cfg(feature = "tls")]
#[derive(Clone)]
pub struct ServerTlsConfig {
    identity: Option<Identity>,
    client_ca_root: Option<Certificate>,
    rustls_raw: Option<tokio_rustls::rustls::ServerConfig>,
}

#[cfg(feature = "tls")]
impl fmt::Debug for ServerTlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTlsConfig").finish()
    }
}

#[cfg(feature = "tls")]
impl ServerTlsConfig {
    /// Creates a new `ServerTlsConfig`.
    pub fn with_rustls() -> Self {
        ServerTlsConfig {
            identity: None,
            client_ca_root: None,
            rustls_raw: None,
        }
    }

    /// Sets the [`Identity`] of the server.
    pub fn identity(self, identity: Identity) -> Self {
        ServerTlsConfig {
            identity: Some(identity),
            ..self
        }
    }

    /// Sets a certificate against which to validate client TLS certificates.
    pub fn client_ca_root(self, cert: Certificate) -> Self {
        ServerTlsConfig {
            client_ca_root: Some(cert),
            ..self
        }
    }

    /// Use options specified by the given `ServerConfig` to configure TLS.
    ///
    /// This overrides all other TLS options set via other means.
    pub fn rustls_server_config(
        &mut self,
        config: tokio_rustls::rustls::ServerConfig,
    ) -> &mut Self {
        self.rustls_raw = Some(config);
        self
    }

    fn tls_acceptor(&self) -> Result<TlsAcceptor, crate::Error> {
        match &self.rustls_raw {
            None => TlsAcceptor::new_with_rustls_identity(
                self.identity.clone().unwrap(),
                self.client_ca_root.clone(),
            ),
            Some(config) => TlsAcceptor::new_with_rustls_raw(config.clone()),
        }
    }
}

#[derive(Debug)]
struct TcpIncoming {
    inner: conn::AddrIncoming,
}

impl TcpIncoming {
    fn bind(addr: SocketAddr) -> Result<Self, crate::Error> {
        let mut inner = conn::AddrIncoming::bind(&addr).map_err(Box::new)?;
        inner.set_nodelay(true);

        Ok(Self { inner })
    }
}

impl Stream for TcpIncoming {
    type Item = Result<conn::AddrStream, crate::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(Accept::poll_accept(Pin::new(&mut self.inner), cx)) {
            Some(Ok(s)) => Poll::Ready(Some(Ok(s))),
            Some(Err(e)) => Poll::Ready(Some(Err(e.into()))),
            None => Poll::Ready(None),
        }
    }
}

#[derive(Debug)]
struct Svc<S>(S);

impl<S> Service<Request<Body>> for Svc<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    type Future = MapErr<S::Future, fn(S::Error) -> crate::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.0.call(req).map_err(|e| e.into())
    }
}

struct MakeSvc<S> {
    interceptor: Option<Interceptor>,
    concurrency_limit: Option<usize>,
    // timeout: Option<Duration>,
    inner: S,
}

impl<S, T> Service<T> for MakeSvc<S>
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

    fn call(&mut self, _: T) -> Self::Future {
        let interceptor = self.interceptor.clone();
        let svc = self.inner.clone();
        let concurrency_limit = self.concurrency_limit;
        // let timeout = self.timeout.clone();

        Box::pin(async move {
            let svc = ServiceBuilder::new()
                .optional_layer(concurrency_limit.map(ConcurrencyLimitLayer::new))
                // .optional_layer(timeout.map(TimeoutLayer::new))
                .service(svc);

            let svc = if let Some(interceptor) = interceptor {
                let layered = interceptor.layer(BoxService::new(Svc(svc)));
                BoxService::new(Svc(layered))
            } else {
                BoxService::new(Svc(svc))
            };

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
