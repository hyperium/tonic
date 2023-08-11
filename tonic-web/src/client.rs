use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{Request, Response, Version};
use http_body::Body;
use pin_project::pin_project;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tower_layer::Layer;
use tower_service::Service;
use tracing::debug;

use crate::call::content_types::GRPC_WEB;
use crate::call::GrpcWebCall;

/// Layer implementing the grpc-web protocol for clients.
#[derive(Debug, Clone)]
pub struct GrpcWebClientLayer {
    _priv: (),
}

impl GrpcWebClientLayer {
    /// Create a new grpc-web for clients layer.
    pub fn new() -> GrpcWebClientLayer {
        Self { _priv: () }
    }
}

impl Default for GrpcWebClientLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for GrpcWebClientLayer {
    type Service = GrpcWebClientService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GrpcWebClientService::new(inner)
    }
}

/// A [`Service`] that wraps some inner http service that will
/// coerce requests coming from [`tonic::client::Grpc`] into proper
/// `grpc-web` requests.
#[derive(Debug, Clone)]
pub struct GrpcWebClientService<S> {
    inner: S,
}

impl<S> GrpcWebClientService<S> {
    /// Create a new grpc-web for clients service.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, B1, B2> Service<Request<B1>> for GrpcWebClientService<S>
where
    S: Service<Request<GrpcWebCall<B1>>, Response = Response<B2>>,
    B1: Body,
    B2: Body<Data = Bytes>,
    B2::Error: Error,
{
    type Response = Response<GrpcWebCall<B2>>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B1>) -> Self::Future {
        if req.version() == Version::HTTP_2 {
            debug!("coercing HTTP2 request to HTTP1.1");

            *req.version_mut() = Version::HTTP_11;
        }

        req.headers_mut()
            .insert(CONTENT_TYPE, GRPC_WEB.try_into().unwrap());

        let req = req.map(GrpcWebCall::client_request);

        let fut = self.inner.call(req);

        ResponseFuture { inner: fut }
    }
}

/// Response future for the [`GrpcWebService`].
#[allow(missing_debug_implementations)]
#[pin_project]
#[must_use = "futures do nothing unless polled"]
pub struct ResponseFuture<F> {
    #[pin]
    inner: F,
}

impl<F, B, E> Future for ResponseFuture<F>
where
    B: Body<Data = Bytes>,
    F: Future<Output = Result<Response<B>, E>>,
{
    type Output = Result<Response<GrpcWebCall<B>>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(self.project().inner.poll(cx));

        Poll::Ready(res.map(|r| r.map(GrpcWebCall::client_response)))
    }
}
