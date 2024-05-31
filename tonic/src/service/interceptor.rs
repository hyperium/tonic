//! gRPC interceptors which are a kind of middleware.
//!
//! See [`Interceptor`] for more details.

use crate::{
    body::{boxed, BoxBody},
    request::SanitizeHeaders,
    Status,
};
use bytes::Bytes;
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_layer::Layer;
use tower_service::Service;

/// A gRPC interceptor.
///
/// gRPC interceptors are similar to middleware but have less flexibility. An interceptor allows
/// you to do two main things, one is to add/remove/check items in the `MetadataMap` of each
/// request. Two, cancel a request with a `Status`.
///
/// Any function that satisfies the bound `FnMut(Request<()>) -> Result<Request<()>, Status>` can be
/// used as an `Interceptor`.
///
/// An interceptor can be used on both the server and client side through the `tonic-build` crate's
/// generated structs.
///
/// See the [interceptor example][example] for more details.
///
/// If you need more powerful middleware, [tower] is the recommended approach. You can find
/// examples of how to use tower with tonic [here][tower-example].
///
/// Additionally, interceptors is not the recommended way to add logging to your service. For that
/// a [tower] middleware is more appropriate since it can also act on the response. For example
/// tower-http's [`Trace`](https://docs.rs/tower-http/latest/tower_http/trace/index.html)
/// middleware supports gRPC out of the box.
///
/// [tower]: https://crates.io/crates/tower
/// [example]: https://github.com/hyperium/tonic/tree/master/examples/src/interceptor
/// [tower-example]: https://github.com/hyperium/tonic/tree/master/examples/src/tower
pub trait Interceptor {
    /// Intercept a request before it is sent, optionally cancelling it.
    fn call(&mut self, request: crate::Request<()>) -> Result<crate::Request<()>, Status>;
}

impl<F> Interceptor for F
where
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
{
    fn call(&mut self, request: crate::Request<()>) -> Result<crate::Request<()>, Status> {
        self(request)
    }
}

/// Create a new interceptor layer.
///
/// See [`Interceptor`] for more details.
pub fn interceptor<F>(f: F) -> InterceptorLayer<F>
where
    F: Interceptor,
{
    InterceptorLayer { f }
}

/// A gRPC interceptor that can be used as a [`Layer`],
/// created by calling [`interceptor`].
///
/// See [`Interceptor`] for more details.
#[derive(Debug, Clone, Copy)]
pub struct InterceptorLayer<F> {
    f: F,
}

impl<S, F> Layer<S> for InterceptorLayer<F>
where
    F: Interceptor + Clone,
{
    type Service = InterceptedService<S, F>;

    fn layer(&self, service: S) -> Self::Service {
        InterceptedService::new(service, self.f.clone())
    }
}

/// A service wrapped in an interceptor middleware.
///
/// See [`Interceptor`] for more details.
#[derive(Clone, Copy)]
pub struct InterceptedService<S, F> {
    inner: S,
    f: F,
}

impl<S, F> InterceptedService<S, F> {
    /// Create a new `InterceptedService` that wraps `S` and intercepts each request with the
    /// function `F`.
    pub fn new(service: S, f: F) -> Self
    where
        F: Interceptor,
    {
        Self { inner: service, f }
    }
}

impl<S, F> fmt::Debug for InterceptedService<S, F>
where
    S: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InterceptedService")
            .field("inner", &self.inner)
            .field("f", &format_args!("{}", std::any::type_name::<F>()))
            .finish()
    }
}

impl<S, F, ReqBody, ResBody> Service<http::Request<ReqBody>> for InterceptedService<S, F>
where
    ResBody: Default + http_body::Body<Data = Bytes> + Send + 'static,
    F: Interceptor,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Error: Into<crate::Error>,
    ResBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Response = http::Response<BoxBody>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        // It is bad practice to modify the body (i.e. Message) of the request via an interceptor.
        // To avoid exposing the body of the request to the interceptor function, we first remove it
        // here, allow the interceptor to modify the metadata and extensions, and then recreate the
        // HTTP request with the body. Tonic requests do not preserve the URI, HTTP version, and
        // HTTP method of the HTTP request, so we extract them here and then add them back in below.
        let uri = req.uri().clone();
        let method = req.method().clone();
        let version = req.version();
        let req = crate::Request::from_http(req);
        let (metadata, extensions, msg) = req.into_parts();

        match self
            .f
            .call(crate::Request::from_parts(metadata, extensions, ()))
        {
            Ok(req) => {
                let (metadata, extensions, _) = req.into_parts();
                let req = crate::Request::from_parts(metadata, extensions, msg);
                let req = req.into_http(uri, method, version, SanitizeHeaders::No);
                ResponseFuture::future(self.inner.call(req))
            }
            Err(status) => ResponseFuture::status(status),
        }
    }
}

// required to use `InterceptedService` with `Router`
impl<S, F> crate::server::NamedService for InterceptedService<S, F>
where
    S: crate::server::NamedService,
{
    const NAME: &'static str = S::NAME;
}

/// Response future for [`InterceptedService`].
#[pin_project]
#[derive(Debug)]
pub struct ResponseFuture<F> {
    #[pin]
    kind: Kind<F>,
}

impl<F> ResponseFuture<F> {
    fn future(future: F) -> Self {
        Self {
            kind: Kind::Future(future),
        }
    }

    fn status(status: Status) -> Self {
        Self {
            kind: Kind::Status(Some(status)),
        }
    }
}

#[pin_project(project = KindProj)]
#[derive(Debug)]
enum Kind<F> {
    Future(#[pin] F),
    Status(Option<Status>),
}

impl<F, E, B> Future for ResponseFuture<F>
where
    F: Future<Output = Result<http::Response<B>, E>>,
    E: Into<crate::Error>,
    B: Default + http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<crate::Error>,
{
    type Output = Result<http::Response<BoxBody>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().kind.project() {
            KindProj::Future(future) => future
                .poll(cx)
                .map(|result| result.map(|res| res.map(boxed))),
            KindProj::Status(status) => {
                let response = status
                    .take()
                    .unwrap()
                    .to_http()
                    .map(|_| B::default())
                    .map(boxed);
                Poll::Ready(Ok(response))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use http::header::HeaderMap;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tower::ServiceExt;

    #[derive(Debug, Default)]
    struct TestBody;

    impl http_body::Body for TestBody {
        type Data = Bytes;
        type Error = Status;

        fn poll_data(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
            Poll::Ready(None)
        }

        fn poll_trailers(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
            Poll::Ready(Ok(None))
        }
    }

    #[tokio::test]
    async fn doesnt_remove_headers_from_requests() {
        let svc = tower::service_fn(|request: http::Request<TestBody>| async move {
            assert_eq!(
                request
                    .headers()
                    .get("user-agent")
                    .expect("missing in leaf service"),
                "test-tonic"
            );

            Ok::<_, Status>(http::Response::new(TestBody))
        });

        let svc = InterceptedService::new(svc, |request: crate::Request<()>| {
            assert_eq!(
                request
                    .metadata()
                    .get("user-agent")
                    .expect("missing in interceptor"),
                "test-tonic"
            );

            Ok(request)
        });

        let request = http::Request::builder()
            .header("user-agent", "test-tonic")
            .body(TestBody)
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn handles_intercepted_status_as_response() {
        let message = "Blocked by the interceptor";
        let expected = Status::permission_denied(message).to_http();

        let svc = tower::service_fn(|_: http::Request<TestBody>| async {
            Ok::<_, Status>(http::Response::new(TestBody))
        });

        let svc = InterceptedService::new(svc, |_: crate::Request<()>| {
            Err(Status::permission_denied(message))
        });

        let request = http::Request::builder().body(TestBody).unwrap();
        let response = svc.oneshot(request).await.unwrap();

        assert_eq!(expected.status(), response.status());
        assert_eq!(expected.version(), response.version());
        assert_eq!(expected.headers(), response.headers());
    }

    #[tokio::test]
    async fn doesnt_change_http_method() {
        let svc = tower::service_fn(|request: http::Request<hyper::Body>| async move {
            assert_eq!(request.method(), http::Method::OPTIONS);

            Ok::<_, hyper::Error>(hyper::Response::new(hyper::Body::empty()))
        });

        let svc = InterceptedService::new(svc, Ok);

        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .body(hyper::Body::empty())
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }
}
