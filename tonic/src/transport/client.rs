use crate::{
    body::BoxBody,
    service::{AddOrigin, BoxService, GrpcService},
};
use futures_util::try_future::{MapErr, TryFutureExt};
use http::Uri;
use hyper::client::conn;
use hyper::client::connect::HttpConnector;
use hyper::client::service::Connect;
use hyper::{Request, Response};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower_buffer::{future::ResponseFuture, Buffer};
use tower_service::Service;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Inner = Box<
    dyn Service<
            Request<BoxBody>,
            Response = Response<hyper::Body>,
            Error = crate::Error,
            Future = BoxFuture<'static, Result<Response<hyper::Body>, crate::Error>>,
        > + Send
        + 'static,
>;

#[derive(Clone)]
pub struct Client {
    svc: Buffer<Inner, Request<BoxBody>>,
}

impl Client {
    pub fn builder() -> Builder {
        Builder::new()
    }
}

impl GrpcService<BoxBody> for Client {
    type ResponseBody = hyper::Body;
    type Error = super::Error;

    type Future = MapErr<
        ResponseFuture<BoxFuture<'static, Result<Response<Self::ResponseBody>, crate::Error>>>,
        fn(crate::Error) -> super::Error,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        GrpcService::poll_ready(&mut self.svc, cx)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        GrpcService::call(&mut self.svc, request)
            .map_err(|e| super::Error::from((super::ErrorKind::Client, e)))
    }
}

#[derive(Debug)]
pub struct Builder {
    ca: Option<Vec<u8>>,
    override_domain: Option<String>,
    buffer_size: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            ca: None,
            override_domain: None,
            buffer_size: 1024,
        }
    }

    #[cfg(any(feature = "openssl-1", feature = "rustls"))]
    pub fn tls(&mut self, ca: Vec<u8>) -> &mut Self {
        self.ca = Some(ca);
        self
    }

    #[cfg(any(feature = "openssl-1", feature = "rustls"))]
    pub fn tls_override_domain<D: AsRef<str>>(&mut self, domain: D) -> &mut Self {
        self.override_domain = Some(domain.as_ref().into());
        self
    }

    pub fn buffer(&mut self, size: usize) -> &mut Self {
        self.buffer_size = size;
        self
    }

    pub fn build<T>(&self, uri: T) -> Result<Client, super::Error>
    where
        Uri: http::HttpTryFrom<T>,
    {
        let uri: Uri = match http::HttpTryFrom::try_from(uri) {
            Ok(u) => u,
            Err(e) => panic!("Invalid uri: {}", e.into()),
        };

        let settings = conn::Builder::new().http2_only(true).clone();

        let svc = if let Some(ca) = &self.ca {
            let domain = self
                .override_domain
                .clone()
                .unwrap_or_else(|| uri.to_string());

            #[cfg(not(any(feature = "openssl-1", feature = "rustls")))]
            panic!("tls configured when no tls implementation feature was selected!");

            #[cfg(feature = "openssl-1")]
            let connector = super::openssl::TlsConnector::new(ca.clone(), domain)?;

            #[cfg(feature = "rustls")]
            #[cfg(not(feature = "openssl-1"))]
            let connector = super::openssl::TlsConnector::new(ca.clone(), domain)?;

            let maker = Connect::new(connector, settings);
            let svc = tower_reconnect::Reconnect::new(maker, uri.clone());

            let svc = AddOrigin::new(svc, uri);
            let svc = BoxService::new(svc);
            Buffer::new(Box::new(svc) as Inner, 100)
        } else {
            let connector = HttpConnector::new();
            let maker = Connect::new(connector, settings);
            let svc = tower_reconnect::Reconnect::new(maker, uri.clone());

            let svc = AddOrigin::new(svc, uri);
            let svc = BoxService::new(svc);
            Buffer::new(Box::new(svc) as Inner, 100)
        };
        // let connector = super::rustls::TlsConnector::load(ca).await?;
        // let connector = super::openssl::TlsConnector::load(ca).await?;

        Ok(Client { svc })
    }
}
