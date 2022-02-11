//! gRPC interceptors which are a kind of middleware.
//!
//! See [`Interceptor`] for more details.

use crate::{request::SanitizeHeaders, Status};
use http::Uri;
use pin_project::pin_project;
use std::{
    fmt,
    future::Future,
    mem,
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

/// Async version of `Interceptor`.
pub trait AsyncInterceptor {
    /// The Future returned by the interceptor.
    type Future: Future<Output = Result<crate::Request<()>, Status>>;
    /// Call the underlying async function that transforms a body-less gRPC request.
    fn call_underlying(&mut self, request: crate::Request<()>) -> Self::Future;
    /// Intercept a request before it is sent, optionally cancelling it.
    fn call<ReqBody>(
        &mut self,
        request: http::Request<ReqBody>,
    ) -> AsyncInterceptorFuture<Self::Future, ReqBody>;
}

impl<F, U> AsyncInterceptor for F
where
    F: FnMut(crate::Request<()>) -> U,
    U: Future<Output = Result<crate::Request<()>, Status>>,
{
    type Future = U;

    fn call_underlying(&mut self, request: crate::Request<()>) -> Self::Future {
        self(request)
    }

    fn call<ReqBody>(
        &mut self,
        request: http::Request<ReqBody>,
    ) -> AsyncInterceptorFuture<U, ReqBody> {
        AsyncInterceptorFuture::new(self, request)
    }
}

/// Wrapper that hides the gRPC body from the underlying [`AsyncInterceptor`] function.
#[pin_project]
#[derive(Debug)]
pub struct AsyncInterceptorFuture<I, ReqBody>
where
    I: Future<Output = Result<crate::Request<()>, Status>>,
{
    #[pin]
    interceptor_fut: I,
    uri: Uri,
    msg: ReqBody,
}

impl<F, ReqBody> AsyncInterceptorFuture<F, ReqBody>
where
    F: Future<Output = Result<crate::Request<()>, Status>>,
{
    fn new<A: AsyncInterceptor<Future = F>>(
        interceptor: &mut A,
        req: http::Request<ReqBody>,
    ) -> Self {
        let uri = req.uri().clone();
        let grpc_req = crate::Request::from_http(req);
        let (metadata, extensions, msg) = grpc_req.into_parts();

        let req_without_body = crate::Request::from_parts(metadata, extensions, ());
        AsyncInterceptorFuture {
            interceptor_fut: interceptor.call_underlying(req_without_body),
            uri,
            msg,
        }
    }
}

impl<F, ReqBody> Future for AsyncInterceptorFuture<F, ReqBody>
where
    F: Future<Output = Result<crate::Request<()>, Status>>,
    ReqBody: Default,
{
    type Output = Result<http::Request<ReqBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.interceptor_fut.poll(cx) {
            Poll::Ready(intercepted_req) => match intercepted_req {
                Ok(r) => {
                    let (metadata, extensions, _) = r.into_parts();
                    let msg = mem::replace(this.msg, ReqBody::default());
                    let req = crate::Request::from_parts(metadata, extensions, msg);
                    let req = req.into_http(this.uri.clone(), SanitizeHeaders::No);
                    Poll::Ready(Ok(req))
                }
                Err(status) => Poll::Ready(Err(status.into())),
            },
            Poll::Pending => return Poll::Pending,
        }
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

/// Create a new async interceptor layer.
///
/// See [`AsyncInterceptor`] and [`Interceptor`] for more details.
pub fn async_interceptor<F>(f: F) -> AsyncInterceptorLayer<F>
where
    F: AsyncInterceptor,
{
    AsyncInterceptorLayer { f }
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

/// A gRPC async interceptor that can be used as a [`Layer`],
/// created by calling [`async_interceptor`].
///
/// See [`AsyncInterceptor`] for more details.
#[derive(Debug, Clone, Copy)]
pub struct AsyncInterceptorLayer<F> {
    f: F,
}

impl<S, F> Layer<S> for AsyncInterceptorLayer<F>
where
    S: Clone,
    F: AsyncInterceptor + Clone,
{
    type Service = AsyncInterceptedService<S, F>;

    fn layer(&self, service: S) -> Self::Service {
        AsyncInterceptedService::new(service, self.f.clone())
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

/// A service wrapped in an async interceptor middleware.
///
/// See [`AsyncInterceptor`] for more details.
#[derive(Clone, Copy)]
pub struct AsyncInterceptedService<S, F> {
    inner: S,
    f: F,
}

impl<S, F> AsyncInterceptedService<S, F> {
    /// Create a new `AsyncInterceptedService` that wraps `S` and intercepts each request with the
    /// function `F`.
    pub fn new(service: S, f: F) -> Self {
        Self { inner: service, f }
    }
}

impl<S, F> fmt::Debug for AsyncInterceptedService<S, F>
where
    S: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncInterceptedService")
            .field("inner", &self.inner)
            .field("f", &format_args!("{}", std::any::type_name::<F>()))
            .finish()
    }
}

impl<S, F, ReqBody, ResBody> Service<http::Request<ReqBody>> for AsyncInterceptedService<S, F>
where
    F: AsyncInterceptor + Clone,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>> + Clone,
    S::Error: Into<crate::Error>,
    ReqBody: Default,
{
    type Response = S::Response;
    type Error = crate::Error;
    type Future = AsyncResponseFuture<S, AsyncInterceptorFuture<F::Future, ReqBody>, ReqBody>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        AsyncResponseFuture::new(self.f.call(req), self.inner.clone())
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

// required to use `AsyncInterceptedService` with `Router`
#[cfg(feature = "transport")]
impl<S, F> crate::transport::NamedService for AsyncInterceptedService<S, F>
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

#[pin_project(project = PinnedOptionProj)]
#[derive(Debug)]
enum PinnedOption<F> {
    Some(#[pin] F),
    None,
}

/// Response future for [`AsyncInterceptedService`].
#[pin_project]
#[derive(Debug)]
pub struct AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>>,
    S::Error: Into<crate::Error>,
    I: Future<Output = Result<http::Request<ReqBody>, crate::Error>>,
{
    #[pin]
    interceptor_fut: PinnedOption<I>,
    #[pin]
    inner_fut: PinnedOption<ResponseFuture<S::Future>>,
    inner: S,
}

impl<S, I, ReqBody> AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>>,
    S::Error: Into<crate::Error>,
    I: Future<Output = Result<http::Request<ReqBody>, crate::Error>>,
{
    fn new(interceptor_fut: I, inner: S) -> Self {
        AsyncResponseFuture {
            interceptor_fut: PinnedOption::Some(interceptor_fut),
            inner_fut: PinnedOption::None,
            inner,
        }
    }
}

impl<S, I, ReqBody, ResBody> Future for AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    I: Future<Output = Result<http::Request<ReqBody>, crate::Error>>,
    S::Error: Into<crate::Error>,
    ReqBody: Default,
{
    type Output = Result<http::Response<ResBody>, crate::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if let PinnedOptionProj::Some(f) = this.interceptor_fut.as_mut().project() {
            match f.poll(cx) {
                Poll::Ready(intercepted_req) => match intercepted_req {
                    Ok(req) => {
                        this.inner_fut
                            .set(PinnedOption::Some(ResponseFuture::future(
                                this.inner.call(req),
                            )));
                        this.interceptor_fut.set(PinnedOption::None);
                    }
                    Err(e) => return Poll::Ready(Err(e)),
                },
                Poll::Pending => return Poll::Pending,
            }
        }
        if let PinnedOptionProj::Some(inner_fut) = this.inner_fut.project() {
            return inner_fut.poll(cx);
        }
        panic!()
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

    #[tokio::test]
    async fn async_interceptor_doesnt_remove_headers() {
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

        let svc = AsyncInterceptedService::new(svc, |request: crate::Request<()>| {
            assert_eq!(
                request
                    .metadata()
                    .get("user-agent")
                    .expect("missing in interceptor"),
                "test-tonic"
            );
            std::future::ready(Ok(request))
        });

        let request = http::Request::builder()
            .header("user-agent", "test-tonic")
            .body(hyper::Body::empty())
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }
}
