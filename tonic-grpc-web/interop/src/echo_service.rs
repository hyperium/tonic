use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::header::{HeaderName, HeaderValue};
use hyper::HeaderMap;
use tonic::body::BoxBody;
use tonic::codegen::{http, HttpBody, Service};
use tonic::transport::NamedService;

type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>>;

#[derive(Clone, Default)]
pub struct EchoHeadersSvc<S> {
    inner: S,
}

impl<S> EchoHeadersSvc<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> Service<http::Request<hyper::Body>> for EchoHeadersSvc<S>
where
    S: Service<http::Request<hyper::Body>, Response = http::Response<BoxBody>> + Send,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: http::Request<hyper::Body>) -> Self::Future {
        let echo_header = req
            .headers()
            .get("x-grpc-test-echo-initial")
            .map(Clone::clone);

        let echo_trailer = req
            .headers()
            .get("x-grpc-test-echo-trailing-bin")
            .map(Clone::clone)
            .map(|v| (HeaderName::from_static("x-grpc-test-echo-trailing-bin"), v));

        let call = self.inner.call(req);

        Box::pin(async move {
            let mut res = call.await?;

            if let Some(echo_header) = echo_header {
                res.headers_mut()
                    .insert("x-grpc-test-echo-initial", echo_header);
                Ok(res
                    .map(|b| MergeTrailers::new(b, echo_trailer))
                    .map(BoxBody::new))
            } else {
                Ok(res)
            }
        })
    }
}

pub struct MergeTrailers<B> {
    inner: B,
    trailer: Option<(HeaderName, HeaderValue)>,
}

impl<B> MergeTrailers<B> {
    pub fn new(inner: B, trailer: Option<(HeaderName, HeaderValue)>) -> Self {
        Self { inner, trailer }
    }
}

impl<B: HttpBody + Unpin> HttpBody for MergeTrailers<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Self::Data, Self::Error>>> {
        Pin::new(&mut self.inner).poll_data(cx)
    }

    fn poll_trailers(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<Option<HeaderMap>, Self::Error>> {
        Pin::new(&mut self.inner).poll_trailers(cx).map_ok(|h| {
            h.map(|mut headers| {
                if let Some((key, value)) = &self.trailer {
                    headers.insert(key.clone(), value.clone());
                }

                headers
            })
        })
    }
}

impl<S: NamedService> NamedService for EchoHeadersSvc<S> {
    const NAME: &'static str = S::NAME;
}
