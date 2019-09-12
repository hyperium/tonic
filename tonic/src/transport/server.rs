use super::{
    service::BoxedIo,
    tls::{Cert, TlsAcceptor},
};
use crate::BoxBody;
use futures_core::Stream;
use futures_util::{try_future::MapOk, TryFutureExt, TryStreamExt, ready};
use http::{Request, Response};
use hyper::server::{conn, accept::Accept};
use hyper::Body;
use std::{net::SocketAddr, task::{Context, Poll}, pin::Pin};
use tower_make::MakeService;
use tower_service::Service;

#[derive(Debug)]
pub struct Server {}

impl Server {
    pub fn builder() -> Builder {
        Builder::new()
    }
}

#[derive(Debug)]
pub struct Builder {
    tls: Option<(Vec<u8>, Vec<u8>)>,
}

impl Builder {
    fn new() -> Self {
        Self { tls: None }
    }

    pub fn tls(&mut self, pem: Vec<u8>, key: Vec<u8>) -> &mut Self {
        self.tls = Some((pem, key));
        self
    }

    // pub fn concurrency_limit(&mut self, limit: usize) -> &mut Self {
    // }

    pub async fn serve<M, S>(self, addr: SocketAddr, svc: M) -> Result<(), super::Error>
    where
        M: Service<(), Response = S>,
        M::Error: Into<crate::Error> + 'static,
        M::Future: Send + 'static,
        S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
        S::Future: Send + 'static,
        S::Error: Into<crate::Error>,
    {
        let tls = if let Some(tls) = self.tls {
            let cert = Cert {
                ca: tls.0,
                key: Some(tls.1),
                domain: String::new(),
            };

            Some(TlsAcceptor::new(cert).unwrap())
        } else {
            None
        };

        let incoming = hyper::server::accept::from_stream(incoming(addr, tls));

        let svc = MakeSvc(svc);

        hyper::Server::builder(incoming)
            .http2_only(true)
            .serve(svc)
            .await
            .unwrap();

        Ok(())
    }
}

fn incoming(
    addr: SocketAddr,
    tls: Option<TlsAcceptor>,
) -> impl futures_core::Stream<Item = Result<BoxedIo, crate::Error>> {
    async_stream::try_stream! {
        let mut tcp = TcpIncoming::bind(addr)?;

        while let Some(stream) = tcp.try_next().await? {
            if let Some(tls) = &tls {
                let io = tls.connect(stream.into_inner()).await?;
                yield BoxedIo::new(io);
            } else {
                yield BoxedIo::new(stream);
            }
        }
    }
}

#[derive(Debug)]
struct TcpIncoming {
    inner: conn::AddrIncoming,
}

impl TcpIncoming {
    fn bind(addr: SocketAddr) -> Result<Self, crate::Error> {
        let inner = conn::AddrIncoming::bind(&addr).map_err(Box::new)?;

        Ok(Self {
            inner,
        })
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
{
    type Response = Response<BoxBody>;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.0.call(req)
    }
}

struct MakeSvc<M>(M);

impl<M, S, T> Service<T> for MakeSvc<M>
where
    M: Service<(), Response = S>,
    M::Error: Into<crate::Error>,
    M::Future: Send + 'static,
    S: Service<Request<Body>, Response = Response<BoxBody>>,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error>,
{
    type Response = Svc<S>;
    type Error = M::Error;
    type Future = MapOk<M::Future, fn(S) -> Svc<S>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        MakeService::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, _: T) -> Self::Future {
        self.0.make_service(()).map_ok(|s| Svc(s))
    }
}
