//! Client implementation and builder.

mod endpoint;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
mod tls;

pub use endpoint::Endpoint;
#[cfg(feature = "tls")]
pub use tls::ClientTlsConfig;

use super::service::{Connection, DynamicServiceStream};
use crate::{
    body::BoxBody,
    codec::compression::{EnabledEncodings, Encoding},
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{
    uri::{InvalidUri, Uri},
    Request, Response,
};
use http_body::Body as _;
use hyper::client::connect::Connection as HyperConnection;
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{channel, Sender},
};

use tower::{
    balance::p2c::Balance,
    buffer::{self, Buffer},
    discover::{Change, Discover},
    util::BoxService,
    Service,
};
use tower_http::set_header::SetRequestHeader;

type Svc = BoxService<Request<BoxBody>, Response<hyper::Body>, crate::Error>;

const DEFAULT_BUFFER_SIZE: usize = 1024;

/// A default batteries included `transport` channel.
///
/// This provides a fully featured http2 gRPC client based on [`hyper::Client`]
/// and `tower` services.
///
/// # Multiplexing requests
///
/// Sending a request on a channel requires a `&mut self` and thus can only send
/// one request in flight. This is intentional and is required to follow the `Service`
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
    /// The encoding that request bodies will be compressed with.
    send_encoding: Option<Encoding>,
}

/// A future that resolves to an HTTP response.
///
/// This is returned by the `Service::call` on [`Channel`].
pub struct ResponseFuture {
    inner: buffer::future::ResponseFuture<<Svc as Service<Request<BoxBody>>>::Future>,
}

impl Channel {
    /// Create an [`Endpoint`] builder that can create [`Channel`]s.
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
        let (channel, tx) = Self::balance_channel(DEFAULT_BUFFER_SIZE);
        list.for_each(|endpoint| {
            tx.try_send(Change::Insert(endpoint.uri.clone(), endpoint))
                .unwrap();
        });

        channel
    }

    /// Balance a list of [`Endpoint`]'s.
    ///
    /// This creates a [`Channel`] that will listen to a stream of change events and will add or remove provided endpoints.
    pub fn balance_channel<K>(capacity: usize) -> (Self, Sender<Change<K, Endpoint>>)
    where
        K: Hash + Eq + Send + Clone + 'static,
    {
        let (tx, rx) = channel(capacity);
        let list = DynamicServiceStream::new(rx);
        (Self::balance(list, DEFAULT_BUFFER_SIZE), tx)
    }

    pub(crate) fn new<C>(connector: C, endpoint: Endpoint) -> Self
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        let buffer_size = endpoint.buffer_size.unwrap_or(DEFAULT_BUFFER_SIZE);
        let accept_encoding = endpoint.accept_encoding;
        let send_encoding = endpoint.send_encoding;

        let svc = Connection::lazy(connector, endpoint);
        let svc = with_accept_encoding(svc, accept_encoding);
        let svc = BoxService::new(svc);
        let svc = Buffer::new(svc, buffer_size);

        Channel { svc, send_encoding }
    }

    pub(crate) async fn connect<C>(connector: C, endpoint: Endpoint) -> Result<Self, super::Error>
    where
        C: Service<Uri> + Send + 'static,
        C::Error: Into<crate::Error> + Send,
        C::Future: Unpin + Send,
        C::Response: AsyncRead + AsyncWrite + HyperConnection + Unpin + Send + 'static,
    {
        let buffer_size = endpoint.buffer_size.unwrap_or(DEFAULT_BUFFER_SIZE);
        let accept_encoding = endpoint.accept_encoding;
        let send_encoding = endpoint.send_encoding;

        let svc = Connection::connect(connector, endpoint)
            .await
            .map_err(super::Error::from_source)?;
        let svc = with_accept_encoding(svc, accept_encoding);
        let svc = BoxService::new(svc);
        let svc = Buffer::new(svc, buffer_size);

        Ok(Channel { svc, send_encoding })
    }

    pub(crate) fn balance<D>(discover: D, buffer_size: usize) -> Self
    where
        D: Discover<Service = Connection> + Unpin + Send + 'static,
        D::Error: Into<crate::Error>,
        D::Key: Hash + Send + Clone,
    {
        let svc = Balance::new(discover);

        let svc = BoxService::new(svc);
        let svc = Buffer::new(svc, buffer_size);

        Channel {
            svc,
            send_encoding: None,
        }
    }
}

fn with_accept_encoding<S>(
    svc: S,
    accept_encoding: EnabledEncodings,
) -> SetRequestHeader<S, http::HeaderValue> {
    let header_value = accept_encoding.into_accept_encoding_header_value();
    SetRequestHeader::overriding(
        svc,
        http::header::HeaderName::from_static(crate::codec::compression::ACCEPT_ENCODING_HEADER),
        header_value,
    )
}

impl Service<http::Request<BoxBody>> for Channel {
    type Response = http::Response<super::Body>;
    type Error = super::Error;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(&mut self.svc, cx).map_err(super::Error::from_source)
    }

    fn call(&mut self, request: http::Request<BoxBody>) -> Self::Future {
        let (mut parts, body) = request.into_parts();

        let new_body = if let Some(encoding) = self.send_encoding {
            parts.headers.insert(
                crate::codec::compression::ENCODING_HEADER,
                encoding.into_header_value(),
            );

            CompressEachChunkBody {
                inner: body,
                encoding,
                encoding_buf: BytesMut::with_capacity(DEFAULT_BUFFER_SIZE),
            }
            .boxed()
        } else {
            body
        };

        let request = http::Request::from_parts(parts, new_body);
        let inner = Service::call(&mut self.svc, request);
        ResponseFuture { inner }
    }
}

impl Future for ResponseFuture {
    type Output = Result<Response<hyper::Body>, super::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let val = futures_util::ready!(Pin::new(&mut self.inner).poll(cx))
            .map_err(super::Error::from_source)?;
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

/// A `http_body::Body` that compresses each chunk with a given encoding.
#[pin_project]
struct CompressEachChunkBody<B> {
    #[pin]
    inner: B,
    encoding: Encoding,
    encoding_buf: BytesMut,
}

impl<B> http_body::Body for CompressEachChunkBody<B>
where
    B: http_body::Body<Data = Bytes, Error = crate::Status>,
{
    type Data = Bytes;
    type Error = crate::Status;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let this = self.project();
        match futures_util::ready!(this.inner.poll_data(cx)) {
            Some(Ok(mut chunk)) => {
                let len = chunk.len();

                this.encoding_buf.clear();

                if let Err(err) = crate::codec::compression::compress(
                    *this.encoding,
                    &mut chunk,
                    this.encoding_buf,
                    len,
                ) {
                    let status =
                        crate::Status::internal("Failed to compress body chunk").with_source(err);
                    return Poll::Ready(Some(Err(status)));
                }

                let chunk = this.encoding_buf.clone().freeze();

                Poll::Ready(Some(Ok(chunk)))
            }
            other => Poll::Ready(other),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        self.project().inner.poll_trailers(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    // we don't define `size_hint` because we compress each
    // chunk and dunno the size
}
