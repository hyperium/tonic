//! gRPC interceptors which are a kind of middleware.
//!
//! See [`Interceptor`] for more details.

use crate::{request::SanitizeHeaders, Status};
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_layer::Layer;
use tower_service::Service;

/// A gRPC incerceptor.
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

#[deprecated(
    since = "0.5.1",
    note = "Please use the `interceptor` function instead"
)]
/// Create a new interceptor layer.
///
/// See [`Interceptor`] for more details.
pub fn interceptor_fn<F>(f: F) -> InterceptorLayer<F>
where
    F: Interceptor,
{
    interceptor(f)
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

#[deprecated(
    since = "0.5.1",
    note = "Please use the `InterceptorLayer` type instead"
)]
/// A gRPC interceptor that can be used as a [`Layer`],
/// created by calling [`interceptor`].
///
/// See [`Interceptor`] for more details.
pub type InterceptorFn<F> = InterceptorLayer<F>;

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
    F: Interceptor,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = http::Response<ResBody>;
    type Error = crate::Error;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let uri = req.uri().clone();
        let req = crate::Request::from_http(req);
        let (metadata, extensions, msg) = req.into_parts();

        match self
            .f
            .call(crate::Request::from_parts(metadata, extensions, ()))
        {
            Ok(req) => {
                let (metadata, extensions, _) = req.into_parts();
                let req = crate::Request::from_parts(metadata, extensions, msg);
                let req = req.into_http(uri, SanitizeHeaders::No);
                ResponseFuture::future(self.inner.call(req))
            }
            Err(status) => ResponseFuture::error(status),
        }
    }
}

// required to use `InterceptedService` with `Router`
#[cfg(feature = "transport")]
impl<S, F> crate::transport::NamedService for InterceptedService<S, F>
where
    S: crate::transport::NamedService,
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

    fn error(status: Status) -> Self {
        Self {
            kind: Kind::Error(Some(status)),
        }
    }
}

#[pin_project(project = KindProj)]
#[derive(Debug)]
enum Kind<F> {
    Future(#[pin] F),
    Error(Option<Status>),
}

impl<F, E, B> Future for ResponseFuture<F>
where
    F: Future<Output = Result<http::Response<B>, E>>,
    E: Into<crate::Error>,
{
    type Output = Result<http::Response<B>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().kind.project() {
            KindProj::Future(future) => future.poll(cx).map_err(Into::into),
            KindProj::Error(status) => {
                let error = status.take().unwrap().into();
                Poll::Ready(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn doesnt_remove_headers() {
        let svc = tower::service_fn(|request: http::Request<hyper::Body>| async move {
            assert_eq!(
                request
                    .headers()
                    .get("user-agent")
                    .expect("missing in leaf service"),
                "test-tonic"
            );

            Ok::<_, hyper::Error>(hyper::Response::new(hyper::Body::empty()))
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
            .body(hyper::Body::empty())
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }
}
