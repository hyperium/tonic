use super::{
    service::BoxedIo,
    tls::{Cert, TlsAcceptor},
};
use crate::BoxBody;
use futures_util::{try_future::MapOk, TryFutureExt, TryStreamExt};
use http::{Request, Response};
use hyper::server::conn;
use hyper::Body;
use std::net::SocketAddr;
use std::task::{Context, Poll};
use tower_make::MakeService;
use tower_service::Service;

pub struct Server {}

impl Server {
    pub fn builder() -> Builder {
        Builder::new()
    }
}

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
        let tcp = conn::AddrIncoming::bind(&addr).unwrap();

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

        let incoming = incoming(tcp, tls);

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
    mut tcp: conn::AddrIncoming,
    tls: Option<TlsAcceptor>,
) -> impl futures_core::Stream<Item = Result<BoxedIo, crate::Error>> {
    async_stream::try_stream! {
        while let Some(stream) = tcp.try_next().await.map_err(Into::into)? {
            if let Some(tls) = &tls {
                let io = tls.connect(stream.into_inner()).await?;
                yield BoxedIo::new(io);
            } else {
                yield BoxedIo::new(stream);
            }
        }
    }
}

// TODO: add custom tracing here
#[derive(Debug)]
pub struct Svc<S>(S);

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

pub struct MakeSvc<M>(M);

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
