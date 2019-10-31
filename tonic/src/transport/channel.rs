//! Client implementation and builder.

use super::{
    service::{Connection, ServiceList},
    Endpoint,
};
use crate::{body::BoxBody, client::GrpcService};
use bytes::Bytes;
use http::{
    uri::{InvalidUriBytes, Uri},
    Request, Response,
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{
    buffer::{self, Buffer},
    discover::Discover,
    util::{BoxService, Either},
    Service,
};
use tower_balance::p2c::Balance;

type Svc = Either<Connection, BoxService<Request<BoxBody>, Response<hyper::Body>, crate::Error>>;

const DEFAULT_BUFFER_SIZE: usize = 1024;

/// A default batteries included `transport` channel.
///
/// This provides a fully featured http2 gRPC client based on [`hyper::Client`]
/// and `tower` services.
#[derive(Clone)]
pub struct Channel {
    svc: Buffer<Svc, Request<BoxBody>>,
    interceptor_headers: Option<Arc<dyn Fn(&mut http::HeaderMap) + Send + Sync + 'static>>,
}

/// A future that resolves to an HTTP response.
///
/// This is returned by the `Service::call` on [`Channel`].
pub struct ResponseFuture {
    inner: buffer::future::ResponseFuture<<Svc as Service<Request<BoxBody>>>::Future>,
}

impl Channel {
    /// Create a [`Endpoint`] builder that can create a [`Channel`]'s.
    pub fn builder(uri: Uri) -> Endpoint {
        Endpoint::from(uri)
    }

    /// Create an `Endpoint` from a static string.
    ///
    /// ```
    /// # use tonic::transport::Channel;
    /// Channel::from_static("https://example.com");
    /// ```
    pub fn from_static(s: &'static str) -> Endpoint {
        let uri = Uri::from_static(s);
        Self::builder(uri)
    }

    /// Create an `Endpoint` from shared bytes.
    ///
    /// ```
    /// # use tonic::transport::Channel;
    /// Channel::from_shared("https://example.com");
    /// ```
    pub fn from_shared(s: impl Into<Bytes>) -> Result<Endpoint, InvalidUriBytes> {
        let uri = Uri::from_shared(s.into())?;
        Ok(Self::builder(uri))
    }

    /// Balance a list of [`Endpoint`]'s.
    ///
    /// This creates a [`Channel`] that will load balance accross all the
    /// provided endpoints.
    pub fn balance_list(list: impl Iterator<Item = Endpoint>) -> Self {
        let list = list.collect::<Vec<_>>();

        let buffer_size = list
            .iter()
            .next()
            .and_then(|e| e.buffer_size)
            .unwrap_or(DEFAULT_BUFFER_SIZE);

        let interceptor_headers = list
            .iter()
            .next()
            .and_then(|e| e.interceptor_headers.clone());

        let discover = ServiceList::new(list);

        Self::balance(discover, buffer_size, interceptor_headers)
    }

    pub(crate) async fn connect(endpoint: Endpoint) -> Result<Self, super::Error> {
        let buffer_size = endpoint.buffer_size.clone().unwrap_or(DEFAULT_BUFFER_SIZE);
        let interceptor_headers = endpoint.interceptor_headers.clone();

        let svc = Connection::new(endpoint)
            .await
            .map_err(|e| super::Error::from_source(super::ErrorKind::Client, e))?;

        let svc = Buffer::new(Either::A(svc), buffer_size);

        Ok(Channel {
            svc,
            interceptor_headers,
        })
    }

    pub(crate) fn balance<D>(
        discover: D,
        buffer_size: usize,
        interceptor_headers: Option<Arc<dyn Fn(&mut http::HeaderMap) + Send + Sync + 'static>>,
    ) -> Self
    where
        D: Discover<Service = Connection> + Unpin + Send + 'static,
        D::Error: Into<crate::Error>,
        D::Key: Send + Clone,
    {
        let svc = Balance::from_entropy(discover);

        let svc = BoxService::new(svc);
        let svc = Buffer::new(Either::B(svc), buffer_size);

        Channel {
            svc,
            interceptor_headers,
        }
    }
}

impl GrpcService<BoxBody> for Channel {
    type ResponseBody = hyper::Body;
    type Error = super::Error;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        GrpcService::poll_ready(&mut self.svc, cx)
            .map_err(|e| super::Error::from_source(super::ErrorKind::Client, e))
    }

    fn call(&mut self, mut request: Request<BoxBody>) -> Self::Future {
        if let Some(interceptor) = self.interceptor_headers.clone() {
            interceptor(request.headers_mut());
        }

        let inner = GrpcService::call(&mut self.svc, request);
        ResponseFuture { inner }
    }
}

impl Future for ResponseFuture {
    type Output = Result<Response<hyper::Body>, super::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let val = futures_util::ready!(Pin::new(&mut self.inner).poll(cx))
            .map_err(|e| super::Error::from_source(super::ErrorKind::Client, e))?;
        Ok(val).into()
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel").finish()
    }
}

impl fmt::Debug for ResponseFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseFuture").finish()
    }
}
