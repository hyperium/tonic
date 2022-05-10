//! gRPC interceptors which are a kind of middleware.
//!
//! See [`Interceptor`] for more details.

use crate::{
    body::{boxed, BoxBody},
    request::SanitizeHeaders,
    Request, Status,
};
use bytes::Bytes;
use http::{Method, Uri, Version};
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
    /// Intercept a request before it is sent, optionally cancelling it.
    fn call(&mut self, request: crate::Request<()>) -> Self::Future;
}

impl<F, U> AsyncInterceptor for F
where
    F: FnMut(crate::Request<()>) -> U,
    U: Future<Output = Result<crate::Request<()>, Status>>,
{
    type Future = U;

    fn call(&mut self, request: crate::Request<()>) -> Self::Future {
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

/// Create a new async interceptor layer.
///
/// See [`AsyncInterceptor`] and [`Interceptor`] for more details.
pub fn async_interceptor<F>(f: F) -> AsyncInterceptorLayer<F>
where
    F: AsyncInterceptor,
{
    AsyncInterceptorLayer { f }
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

// Components and attributes of a request, without metadata or extensions.
#[derive(Debug)]
struct DecomposedRequest<ReqBody> {
    uri: Uri,
    method: Method,
    http_version: Version,
    msg: ReqBody,
}

/// Decompose the request into its contents and properties, and create a new request without a body.
///
/// It is bad practice to modify the body (i.e. Message) of the request via an interceptor.
/// To avoid exposing the body of the request to the interceptor function, we first remove it
/// here, allow the interceptor to modify the metadata and extensions, and then recreate the
/// HTTP request with the original message body with the `recompose` function. Also note that Tonic
/// requests do not preserve the URI, HTTP version, and HTTP method of the HTTP request, so we
/// extract them here and then add them back in `recompose`.
fn decompose<ReqBody>(req: http::Request<ReqBody>) -> (DecomposedRequest<ReqBody>, Request<()>) {
    let uri = req.uri().clone();
    let method = req.method().clone();
    let http_version = req.version();
    let req = crate::Request::from_http(req);
    let (metadata, extensions, msg) = req.into_parts();

    let dreq = DecomposedRequest {
        uri,
        method,
        http_version,
        msg,
    };
    let req_without_body = crate::Request::from_parts(metadata, extensions, ());

    (dreq, req_without_body)
}

/// Combine the modified metadata and extensions with the original message body and attributes.
fn recompose<ReqBody>(
    dreq: DecomposedRequest<ReqBody>,
    modified_req: Request<()>,
) -> http::Request<ReqBody> {
    let (metadata, extensions, _) = modified_req.into_parts();
    let req = crate::Request::from_parts(metadata, extensions, dreq.msg);

    req.into_http(
        dreq.uri,
        dreq.method,
        dreq.http_version,
        SanitizeHeaders::No,
    )
}

impl<S, F, ReqBody, ResBody> Service<http::Request<ReqBody>> for InterceptedService<S, F>
where
    F: Interceptor,
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    S::Error: Into<crate::Error>,
    ResBody: Default + http_body::Body<Data = Bytes> + Send + 'static,
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
        let (dreq, req_without_body) = decompose(req);

        match self.f.call(req_without_body) {
            Ok(modified_req) => {
                let modified_req_with_body = recompose(dreq, modified_req);

                ResponseFuture::future(self.inner.call(modified_req_with_body))
            }
            Err(status) => ResponseFuture::status(status),
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
    ResBody: Default + http_body::Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Response = http::Response<BoxBody>;
    type Error = S::Error;
    type Future = AsyncResponseFuture<S, F::Future, ReqBody>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        AsyncResponseFuture::new(req, &mut self.f, self.inner.clone())
    }
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

#[pin_project(project = PinnedOptionProj)]
#[derive(Debug)]
enum PinnedOption<F> {
    Some(#[pin] F),
    None,
}

/// Response future for [`AsyncInterceptedService`].
///
/// Handles the call to the async interceptor, then calls the inner service and wraps the result in
/// [`ResponseFuture`].
#[pin_project(project = AsyncResponseFutureProj)]
#[derive(Debug)]
pub struct AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>>,
    S::Error: Into<crate::Error>,
    I: Future<Output = Result<crate::Request<()>, Status>>,
{
    #[pin]
    interceptor_fut: PinnedOption<I>,
    #[pin]
    inner_fut: PinnedOption<ResponseFuture<S::Future>>,
    inner: S,
    dreq: DecomposedRequest<ReqBody>,
}

impl<S, I, ReqBody> AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>>,
    S::Error: Into<crate::Error>,
    I: Future<Output = Result<crate::Request<()>, Status>>,
    ReqBody: Default,
{
    fn new<A: AsyncInterceptor<Future = I>>(
        req: http::Request<ReqBody>,
        interceptor: &mut A,
        inner: S,
    ) -> Self {
        let (dreq, req_without_body) = decompose(req);
        let interceptor_fut = interceptor.call(req_without_body);

        AsyncResponseFuture {
            interceptor_fut: PinnedOption::Some(interceptor_fut),
            inner_fut: PinnedOption::None,
            inner,
            dreq,
        }
    }

    /// Calls the inner service with the intercepted request (which has been modified by the
    /// async interceptor func).
    fn create_inner_fut(
        this: &mut AsyncResponseFutureProj<'_, S, I, ReqBody>,
        intercepted_req: Result<crate::Request<()>, Status>,
    ) -> ResponseFuture<S::Future> {
        match intercepted_req {
            Ok(req) => {
                // We can't move the message body out of the pin projection. So, to
                // avoid copying it, we swap its memory with an empty body and then can
                // move it into the recomposed request.
                let msg = mem::take(&mut this.dreq.msg);
                let movable_dreq = DecomposedRequest {
                    uri: this.dreq.uri.clone(),
                    method: this.dreq.method.clone(),
                    http_version: this.dreq.http_version,
                    msg,
                };
                let modified_req_with_body = recompose(movable_dreq, req);

                ResponseFuture::future(this.inner.call(modified_req_with_body))
            }
            Err(status) => ResponseFuture::status(status),
        }
    }
}

impl<S, I, ReqBody, ResBody> Future for AsyncResponseFuture<S, I, ReqBody>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
    I: Future<Output = Result<crate::Request<()>, Status>>,
    S::Error: Into<crate::Error>,
    ReqBody: Default,
    ResBody: Default + http_body::Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<crate::Error>,
{
    type Output = Result<http::Response<BoxBody>, S::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        // The struct was initialized (via `new`) with interceptor func future, which we poll here.
        if let PinnedOptionProj::Some(f) = this.interceptor_fut.as_mut().project() {
            match f.poll(cx) {
                Poll::Ready(intercepted_req) => {
                    let inner_fut = AsyncResponseFuture::<S, I, ReqBody>::create_inner_fut(
                        &mut this,
                        intercepted_req,
                    );
                    // Set the inner service future and clear the interceptor future.
                    this.inner_fut.set(PinnedOption::Some(inner_fut));
                    this.interceptor_fut.set(PinnedOption::None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        // At this point, inner_fut should always be Some.
        let inner_fut = match this.inner_fut.project() {
            PinnedOptionProj::None => panic!(),
            PinnedOptionProj::Some(f) => f,
        };

        inner_fut.poll(cx)
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
    async fn async_interceptor_handles_intercepted_status_as_response() {
        let message = "Blocked by the interceptor";
        let expected = Status::permission_denied(message).to_http();

        let svc = tower::service_fn(|_: http::Request<TestBody>| async {
            Ok::<_, Status>(http::Response::new(TestBody))
        });

        let svc = AsyncInterceptedService::new(svc, |_: crate::Request<()>| {
            std::future::ready(Err(Status::permission_denied(message)))
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

        let svc = InterceptedService::new(svc, |request: crate::Request<()>| Ok(request));

        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .body(hyper::Body::empty())
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }

    #[tokio::test]
    async fn async_interceptor_doesnt_change_http_method() {
        let svc = tower::service_fn(|request: http::Request<hyper::Body>| async move {
            assert_eq!(request.method(), http::Method::OPTIONS);

            Ok::<_, hyper::Error>(hyper::Response::new(hyper::Body::empty()))
        });

        let svc = AsyncInterceptedService::new(svc, |request: crate::Request<()>| {
            std::future::ready(Ok(request))
        });

        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .body(hyper::Body::empty())
            .unwrap();

        svc.oneshot(request).await.unwrap();
    }
}
