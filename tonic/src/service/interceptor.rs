//! gRPC interceptors which are a kind of middleware.

use crate::Status;
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_layer::Layer;
use tower_service::Service;

/// Create a new interceptor from a function.
///
/// gRPC interceptors are similar to middleware but have less flexibility. This interceptor allows
/// you to do two main things, one is to add/remove/check items in the `MetadataMap` of each
/// request. Two, cancel a request with any `Status`.
///
/// An interceptor can be used on both the server and client side through the `tonic-build` crate's
/// generated structs.
///
/// These interceptors do not allow you to modify the `Message` of the request but allow you to
/// check for metadata. If you would like to apply middleware like features to the body of the
/// request, going through the [tower] abstraction is recommended.
///
/// Interceptors is not recommend should not be used to add logging to your service. For that a
/// [tower] middleware is more appropriate since it can also act on the response.
///
/// See the [interceptor example][example] for more details.
///
/// [tower]: https://crates.io/crates/tower
/// [example]: https://github.com/hyperium/tonic/tree/master/examples/src/interceptor
// TODO: when tower-http is shipped update the docs to mention its `Trace` middleware which has
// support for gRPC and is an easy to add logging
pub fn interceptor_fn<F>(f: F) -> InterceptorFn<F>
where
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
{
    InterceptorFn { f }
}

/// An interceptor created from a function.
///
/// See [`interceptor_fn`] for more details.
#[derive(Debug, Clone, Copy)]
pub struct InterceptorFn<F> {
    f: F,
}

impl<S, F> Layer<S> for InterceptorFn<F>
where
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status> + Clone,
{
    type Service = InterceptedService<S, F>;

    fn layer(&self, service: S) -> Self::Service {
        InterceptedService::new(service, self.f.clone())
    }
}

/// A service wrapped in an interceptor middleware.
///
/// See [`interceptor_fn`] for more details.
#[derive(Clone, Copy)]
pub struct InterceptedService<S, F> {
    inner: S,
    f: F,
}

impl<S, F> InterceptedService<S, F> {
    /// Create a new `InterceptedService` thats wraps `S` and intercepts each request with the
    /// function `F`.
    pub fn new(service: S, f: F) -> Self
    where
        F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
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
    F: FnMut(crate::Request<()>) -> Result<crate::Request<()>, Status>,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = http::Response<ResBody>;
    type Error = crate::Error;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let uri = req.uri().clone();
        let req = crate::Request::from_http(req);
        let (metadata, extensions, msg) = req.into_parts();

        match (self.f)(crate::Request::from_parts(metadata, extensions, ())) {
            Ok(req) => {
                let (metadata, extensions, _) = req.into_parts();
                let req = crate::Request::from_parts(metadata, extensions, msg);
                let req = req.into_http(uri);
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
            KindProj::Future(future) => {
                let response = futures_core::ready!(future.poll(cx).map_err(Into::into)?);
                Poll::Ready(Ok(response))
            }
            KindProj::Error(status) => {
                let error = status.take().unwrap().into();
                Poll::Ready(Err(error))
            }
        }
    }
}
