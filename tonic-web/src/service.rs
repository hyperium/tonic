use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Buf;
use http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Version};
use http_body::Body;
use pin_project::pin_project;
use tonic::body::{empty_body, BoxBody};
use tonic::server::NamedService;
use tower_service::Service;
use tracing::{debug, trace};

use crate::call::content_types::is_grpc_web;
use crate::call::{Encoding, GrpcWebCall};
use crate::{box_body, BoxError};

const GRPC: &str = "application/grpc";

/// Service implementing the grpc-web protocol.
#[derive(Debug)]
pub struct GrpcWebService<S, ResBody = BoxBody> {
    inner: S,
    _markers: std::marker::PhantomData<ResBody>,
}

#[derive(Debug, PartialEq)]
enum RequestKind<'a> {
    // The request is considered a grpc-web request if its `content-type`
    // header is exactly one of:
    //
    //  - "application/grpc-web"
    //  - "application/grpc-web+proto"
    //  - "application/grpc-web-text"
    //  - "application/grpc-web-text+proto"
    GrpcWeb {
        method: &'a Method,
        encoding: Encoding,
        accept: Encoding,
    },
    // All other requests, including `application/grpc`
    Other(http::Version),
}

impl<S: Clone, ResBody> Clone for GrpcWebService<S, ResBody> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _markers: std::marker::PhantomData,
        }
    }
}

impl<S, ResBody> GrpcWebService<S, ResBody> {
    pub(crate) fn new(inner: S) -> Self {
        GrpcWebService {
            inner,
            _markers: std::marker::PhantomData,
        }
    }
}

impl<S, ResBody> GrpcWebService<S, ResBody> {
    fn response<ReqBody>(&self, status: StatusCode) -> ResponseFuture<S::Future>
    where
        S: Service<Request<ReqBody>, Response = Response<ResBody>> + Send + 'static,
        ResBody: Body,
    {
        ResponseFuture {
            case: Case::ImmediateResponse {
                res: Some(
                    Response::builder()
                        .status(status)
                        .body(empty_body())
                        .unwrap(),
                ),
            },
        }
    }
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for GrpcWebService<S, ResBody>
where
    S: Service<Request<BoxBody>, Response = Response<ResBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ReqBody: Body + Send + 'static,
    ReqBody::Error: Error + Send + Sync,
    ResBody: Body + Send + 'static,
    ResBody::Error: Error + Send + Sync + 'static,
{
    type Response = Response<BoxBody>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        match RequestKind::new(req.headers(), req.method(), req.version()) {
            // A valid grpc-web request, regardless of HTTP version.
            //
            // If the request includes an `origin` header, we verify it is allowed
            // to access the resource, an HTTP 403 response is returned otherwise.
            //
            // If the origin is allowed to access the resource or there is no
            // `origin` header present, translate the request into a grpc request,
            // call the inner service, and translate the response back to
            // grpc-web.
            RequestKind::GrpcWeb {
                method: &Method::POST,
                encoding,
                accept,
            } => {
                trace!(kind = "simple", path = ?req.uri().path(), ?encoding, ?accept);

                ResponseFuture {
                    case: Case::GrpcWeb {
                        future: self.inner.call(coerce_request(req, encoding).map(box_body)),
                        accept,
                    },
                }
            }

            // The request's content-type matches one of the 4 supported grpc-web
            // content-types, but the request method is not `POST`.
            // This is not a valid grpc-web request, return HTTP 405.
            RequestKind::GrpcWeb { .. } => {
                debug!(kind = "simple", error="method not allowed", method = ?req.method());
                self.response(StatusCode::METHOD_NOT_ALLOWED)
            }

            // All http/2 requests that are not grpc-web are passed through to the inner service,
            // whatever they are.
            RequestKind::Other(Version::HTTP_2) => {
                debug!(kind = "other h2", content_type = ?req.headers().get(header::CONTENT_TYPE));
                ResponseFuture {
                    case: Case::Other {
                        future: self.inner.call(req.map(box_body)),
                    },
                }
            }

            // Return HTTP 400 for all other requests.
            RequestKind::Other(_) => {
                debug!(kind = "other h1", content_type = ?req.headers().get(header::CONTENT_TYPE));
                self.response(StatusCode::BAD_REQUEST)
            }
        }
    }
}

/// Response future for the [`GrpcWebService`].
#[allow(missing_debug_implementations)]
#[pin_project]
#[must_use = "futures do nothing unless polled"]
pub struct ResponseFuture<F> {
    #[pin]
    case: Case<F>,
}

#[pin_project(project = CaseProj)]
enum Case<F> {
    GrpcWeb {
        #[pin]
        future: F,
        accept: Encoding,
    },
    Other {
        #[pin]
        future: F,
    },
    ImmediateResponse {
        res: Option<Response<BoxBody>>,
    },
}

impl<F, E, ResBody> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>> + Send + 'static,
    ResBody: Body + Send + 'static,
    ResBody::Error: Error + Send + Sync,
    E: Into<BoxError> + Send,
{
    type Output = Result<Response<BoxBody>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        match this.case.as_mut().project() {
            CaseProj::GrpcWeb { future, accept } => {
                let res = ready!(future.poll(cx))?;

                Poll::Ready(Ok(coerce_response(res, *accept).map(box_body)))
            }
            CaseProj::Other { future } => future.poll(cx).map_ok(|res| res.map(box_body)),
            CaseProj::ImmediateResponse { res } => Poll::Ready(Ok(res.take().unwrap())),
        }
    }
}

impl<S: NamedService, ResBody> NamedService for GrpcWebService<S, ResBody> {
    const NAME: &'static str = S::NAME;
}

impl<'a> RequestKind<'a> {
    fn new(headers: &'a HeaderMap, method: &'a Method, version: Version) -> Self {
        if is_grpc_web(headers) {
            return RequestKind::GrpcWeb {
                method,
                encoding: Encoding::from_content_type(headers),
                accept: Encoding::from_accept(headers),
            };
        }

        RequestKind::Other(version)
    }
}

// Mutating request headers to conform to a gRPC request is not really
// necessary for us at this point. We could remove most of these except
// maybe for inserting `header::TE`, which tonic should check?
fn coerce_request<ReqBody: Body + Send + 'static>(
    mut req: Request<ReqBody>,
    encoding: Encoding,
) -> Request<GrpcWebCall<ReqBody>> {
    req.headers_mut().remove(header::CONTENT_LENGTH);

    req.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(GRPC));

    req.headers_mut()
        .insert(header::TE, HeaderValue::from_static("trailers"));

    req.headers_mut().insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity,deflate,gzip"),
    );

    req.map(|b| GrpcWebCall::request(b, encoding))
}

fn coerce_response<ResBody, D>(
    res: Response<ResBody>,
    encoding: Encoding,
) -> Response<GrpcWebCall<ResBody>>
where
    ResBody: Body<Data = D> + Send + 'static,
    D: Buf,
{
    let mut res = res.map(|b| GrpcWebCall::response(b, encoding));

    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(encoding.to_content_type()),
    );

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::content_types::*;
    use http::header::{
        ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, CONTENT_TYPE, ORIGIN,
    };

    type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

    #[derive(Debug, Clone)]
    struct Svc;

    impl<B> tower_service::Service<Request<B>> for Svc {
        type Response = Response<BoxBody>;
        type Error = String;
        type Future = BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request<B>) -> Self::Future {
            Box::pin(async { Ok(Response::new(empty_body())) })
        }
    }

    impl NamedService for Svc {
        const NAME: &'static str = "test";
    }

    mod grpc_web {
        use super::*;
        use http::HeaderValue;
        use tower_layer::Layer;

        fn request() -> Request<hyper::Body> {
            Request::builder()
                .method(Method::POST)
                .header(CONTENT_TYPE, GRPC_WEB)
                .header(ORIGIN, "http://example.com")
                .body(hyper::Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn default_cors_config() {
            let mut svc = crate::enable(Svc);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn web_layer() {
            let mut svc = crate::GrpcWebLayer::new().layer(Svc);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn without_origin() {
            let mut svc = crate::enable(Svc);

            let mut req = request();
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn only_post_and_options_allowed() {
            let mut svc = crate::enable(Svc);

            for method in &[
                Method::GET,
                Method::PUT,
                Method::DELETE,
                Method::HEAD,
                Method::PATCH,
            ] {
                let mut req = request();
                *req.method_mut() = method.clone();

                let res = svc.call(req).await.unwrap();

                assert_eq!(
                    res.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{} should not be allowed",
                    method
                );
            }
        }

        #[tokio::test]
        async fn grpc_web_content_types() {
            let mut svc = crate::enable(Svc);

            for ct in &[GRPC_WEB_TEXT, GRPC_WEB_PROTO, GRPC_WEB_TEXT_PROTO, GRPC_WEB] {
                let mut req = request();
                req.headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static(ct));

                let res = svc.call(req).await.unwrap();

                assert_eq!(res.status(), StatusCode::OK);
            }
        }
    }

    mod options {
        use super::*;

        fn request() -> Request<hyper::Body> {
            Request::builder()
                .method(Method::OPTIONS)
                .header(ORIGIN, "http://example.com")
                .header(ACCESS_CONTROL_REQUEST_HEADERS, "x-grpc-web")
                .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(hyper::Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn valid_grpc_web_preflight() {
            let mut svc = crate::enable(Svc);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }
    }

    mod grpc {
        use super::*;
        use http::HeaderValue;

        fn request() -> Request<hyper::Body> {
            Request::builder()
                .version(Version::HTTP_2)
                .header(CONTENT_TYPE, GRPC)
                .body(hyper::Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn h2_is_ok() {
            let mut svc = crate::enable(Svc);

            let req = request();
            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK)
        }

        #[tokio::test]
        async fn h1_is_err() {
            let mut svc = crate::enable(Svc);

            let req = Request::builder()
                .header(CONTENT_TYPE, GRPC)
                .body(hyper::Body::empty())
                .unwrap();

            let res = svc.call(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST)
        }

        #[tokio::test]
        async fn content_type_variants() {
            let mut svc = crate::enable(Svc);

            for variant in &["grpc", "grpc+proto", "grpc+thrift", "grpc+foo"] {
                let mut req = request();
                req.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_maybe_shared(format!("application/{}", variant)).unwrap(),
                );

                let res = svc.call(req).await.unwrap();

                assert_eq!(res.status(), StatusCode::OK)
            }
        }
    }

    mod other {
        use super::*;

        fn request() -> Request<hyper::Body> {
            Request::builder()
                .header(CONTENT_TYPE, "application/text")
                .body(hyper::Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn h1_is_err() {
            let mut svc = crate::enable(Svc);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST)
        }

        #[tokio::test]
        async fn h2_is_ok() {
            let mut svc = crate::enable(Svc);
            let mut req = request();
            *req.version_mut() = Version::HTTP_2;

            let res = svc.call(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK)
        }
    }
}
