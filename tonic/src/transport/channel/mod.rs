//! Client implementation and builder.

mod endpoint;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
mod tls;

pub use endpoint::Endpoint;
#[cfg(feature = "tls")]
pub use tls::ClientTlsConfig;

use super::service::{Connection, ServiceList};
use crate::{body::BoxBody, client::GrpcService};
use bytes::Bytes;
use http::{
    uri::{InvalidUri, Uri},
    Request, Response,
};
use hyper::client::connect::Connection as HyperConnection;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};
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
///
/// # Multiplexing requests
///
/// Sending a request on a channel requires a `&mut self` and thus can only send
/// on request in flight. This is intentional and is required to follow the `Service`
/// contract from the `tower` library which this channel implementation is built on
/// top of.
///
/// `tower` itself has a concept of `poll_ready` which is the main mechanism to apply
/// back pressure. `poll_ready` takes a `&mut self` and when it returns `Poll::Ready`
/// we know the `Service` is able to accept only one request before we must `poll_ready`
/// again. Due to this fact any `async fn` that wants to poll for readiness and submit
/// the request must have a `&mut self` reference.
///
/// To work around this and to ease the use of the channel, `Channel` provides a
/// `Clone` implementation that is _cheap_. This is because at the very top level
/// the channel is backed by a `tower_buffer::Buffer` which runs the connection
/// in a background task and provides a `mpsc` channel interface. Due to this
/// cloning the `Channel` type is cheap and encouraged.
#[derive(Clone)]
pub struct Channel {
    svc: Buffer<Svc, Request<BoxBody>>,
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
    pub fn from_shared(s: impl Into<Bytes>) -> Result<Endpoint, InvalidUri> {
        let uri = Uri::from_maybe_shared(s.into())?;
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

        let discover = ServiceList::new(list);

        Self::balance(discover, buffer_size)
    }

    pub(crate) async fn connect<C>(connector: C, endpoint: Endpoint) -> Result<Self, super::Error>
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        let buffer_size = endpoint.buffer_size.clone().unwrap_or(DEFAULT_BUFFER_SIZE);

        let svc = Connection::new(connector, endpoint)
            .await
            .map_err(|e| super::Error::from_source(e))?;

        let svc = Buffer::new(Either::A(svc), buffer_size);

        Ok(Channel { svc })
    }

    pub(crate) fn balance<D>(discover: D, buffer_size: usize) -> Self
    where
        D: Discover<Service = Connection> + Unpin + Send + 'static,
        D::Error: Into<crate::Error>,
        D::Key: Send + Clone,
    {
        let svc = Balance::from_entropy(discover);

        let svc = BoxService::new(svc);
        let svc = Buffer::new(Either::B(svc), buffer_size);

        Channel { svc }
    }
}

impl GrpcService<BoxBody> for Channel {
    type ResponseBody = hyper::Body;
    type Error = super::Error;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        GrpcService::poll_ready(&mut self.svc, cx).map_err(|e| super::Error::from_source(e))
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        let inner = GrpcService::call(&mut self.svc, request);
        ResponseFuture { inner }
    }
}

impl Future for ResponseFuture {
    type Output = Result<Response<hyper::Body>, super::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let val = futures_util::ready!(Pin::new(&mut self.inner).poll(cx))
            .map_err(|e| super::Error::from_source(e))?;
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
