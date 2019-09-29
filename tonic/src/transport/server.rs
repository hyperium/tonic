//! Server implementation and builder.

use super::service::{layer_fn, BoxedIo, ServiceBuilderExt};
#[cfg(feature = "tls")]
use super::{service::TlsAcceptor, tls::Identity};
use crate::body::BoxBody;
use futures_core::Stream;
use futures_util::{ready, try_future::MapErr, TryFutureExt, TryStreamExt};
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
    layer::{util::Stack, Layer},
    limit::concurrency::ConcurrencyLimitLayer,
    // timeout::TimeoutLayer,
    Service,
    ServiceBuilder,
};
use tower_make::MakeService;

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
}

impl Server {
    /// Create a new server builder that can configure a [`Server`].
    pub fn builder() -> Self {
        Default::default()
    }
}

impl Server {
    /// Set the [`Identity`] of this server using `openssl`.
    ///
    /// ```no_run
    /// # use tonic::transport::{Identity, Server};
    /// # fn dothing() -> Result<(),  Box<dyn std::error::Error>> {
    /// # let mut builder = Server::builder();
    /// let cert = std::fs::read_to_string("server.pem")?;
    /// let key = std::fs::read_to_string("server.key")?;
    ///
    /// let identity = Identity::from_pem(&cert, &key);
    ///
    /// builder.openssl_tls(identity);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "openssl")]
    pub fn openssl_tls(&mut self, identity: Identity) -> &mut Self {
        let acceptor = TlsAcceptor::new_with_openssl(identity).unwrap();
        self.tls = Some(acceptor);
        self
    }

    /// Set the [`Identity`] of this server using `rustls`.
    ///
    /// ```no_run
    /// # use tonic::transport::{Identity, Server};
    /// # fn dothing() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut builder = Server::builder();
    /// let cert = std::fs::read_to_string("server.pem")?;
    /// let key = std::fs::read_to_string("server.key")?;
    ///
    /// let identity = Identity::from_pem(&cert, &key);
    ///
    /// builder.rustls_tls(identity);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "rustls")]
    pub fn rustls_tls(&mut self, identity: Identity) -> &mut Self {
        let acceptor = TlsAcceptor::new_with_rustls(identity).unwrap();
        self.tls = Some(acceptor);
        self
    }

    /// Set the concurrency limit applied to on requests inbound per connection.
    ///
    /// ```
    /// # use tonic::transport::Server;
    /// # use tower_service::Service;
    /// # let mut builder = Server::builder();
    /// builder.concurrency_limit_per_connection(32);
    /// ```
    pub fn concurrency_limit_per_connection(&mut self, limit: usize) -> &mut Self {
        self.concurrency_limit = Some(limit);
        self
    }

    // FIXME: tower-timeout currentlly uses `From` instead of `Into` for the error
    // so our services do not align.
    // pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
    //     self.timeout = Some(timeout);
    //     self
    // }

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
    pub fn interceptor_fn<F, Out>(&mut self, f: F) -> &mut Self
    where
        F: Fn(&mut BoxService, Request<Body>) -> Out + Send + Sync + 'static,
        Out: Future<Output = Result<Response<BoxBody>, crate::Error>> + Send + 'static,
    {
        let f = Arc::new(f);
        let interceptor = layer_fn(move |mut s| {
            let f = f.clone();
            tower::service_fn(move |req| f(&mut s, req))
        });
        let layer = Stack::new(interceptor, layer_fn(|s| BoxService::new(s)));
        self.interceptor = Some(Arc::new(layer));
        self
    }

    /// Consume this [`Server`] creating a future that will execute the server
    /// on [`tokio`]'s default executor.
    pub async fn serve<M, S>(self, addr: SocketAddr, svc: M) -> Result<(), super::Error>
    where
        M: Service<(), Response = S>,
        M::Error: Into<crate::Error> + Send + 'static,
        M::Future: Send + 'static,
        S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error> + Send,
    {
        let interceptor = self.interceptor.clone();
        let concurrency_limit = self.concurrency_limit.clone();
        // let timeout = self.timeout.clone();

        let incoming = hyper::server::accept::from_stream(async_stream::try_stream! {
            let mut tcp = TcpIncoming::bind(addr)?;

            while let Some(stream) = tcp.try_next().await? {
                #[cfg(feature = "tls")]
                {
                    if let Some(tls) = &self.tls {
                        let io = tls.connect(stream.into_inner()).await?;
                        yield BoxedIo::new(io);
                        continue;
                    }
                }

                yield BoxedIo::new(stream);
            }
        });

        let svc = MakeSvc {
            inner: svc,
            interceptor,
            concurrency_limit,
            // timeout,
        };

        hyper::Server::builder(incoming)
            .http2_only(true)
            .serve(svc)
            .await
            .map_err(map_err)?;

        Ok(())
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

#[derive(Debug)]
struct TcpIncoming {
    inner: conn::AddrIncoming,
}

impl TcpIncoming {
    fn bind(addr: SocketAddr) -> Result<Self, crate::Error> {
        let inner = conn::AddrIncoming::bind(&addr).map_err(Box::new)?;

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

// TODO: add custom tracing here
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

struct MakeSvc<M> {
    interceptor: Option<Interceptor>,
    concurrency_limit: Option<usize>,
    // timeout: Option<Duration>,
    inner: M,
}

impl<M, S, T> Service<T> for MakeSvc<M>
where
    M: Service<(), Response = S>,
    M::Error: Into<crate::Error> + Send,
    M::Future: Send + 'static,
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
{
    type Response = BoxService;
    type Error = crate::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeService::poll_ready(&mut self.inner, cx).map_err(Into::into)
    }

    fn call(&mut self, _: T) -> Self::Future {
        let interceptor = self.interceptor.clone();
        let make = self.inner.make_service(());
        let concurrency_limit = self.concurrency_limit.clone();
        // let timeout = self.timeout.clone();

        Box::pin(async move {
            let svc = make.await.map_err(Into::into)?;

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
