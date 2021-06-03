use std::task::{Context, Poll};

use http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Version};
use hyper::Body;
use tonic::body::{empty_body, BoxBody};
use tonic::transport::NamedService;
use tower_service::Service;
use tracing::{debug, trace};

use crate::call::content_types::is_grpc_web;
use crate::call::{Encoding, GrpcWebCall};
use crate::cors::Cors;
use crate::cors::{ORIGIN, REQUEST_HEADERS};
use crate::{BoxError, BoxFuture, Config};

const GRPC: &str = "application/grpc";

#[derive(Debug, Clone)]
pub struct GrpcWeb<S> {
    inner: S,
    cors: Cors,
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
    // The request is considered a grpc-web preflight request if all these
    // conditions are met:
    //
    // - the request method is `OPTIONS`
    // - request headers include `origin`
    // - `access-control-request-headers` header is present and includes `x-grpc-web`
    GrpcWebPreflight {
        origin: &'a HeaderValue,
        request_headers: &'a HeaderValue,
    },
    // All other requests, including `application/grpc`
    Other(http::Version),
}

impl<S> GrpcWeb<S> {
    pub(crate) fn new(inner: S, config: Config) -> Self {
        GrpcWeb {
            inner,
            cors: Cors::new(config),
        }
    }
}

impl<S> GrpcWeb<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
{
    fn no_content(&self, headers: HeaderMap) -> BoxFuture<S::Response, S::Error> {
        let mut res = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(empty_body())
            .unwrap();

        res.headers_mut().extend(headers);

        Box::pin(async { Ok(res) })
    }

    fn response(&self, status: StatusCode) -> BoxFuture<S::Response, S::Error> {
        Box::pin(async move {
            Ok(Response::builder()
                .status(status)
                .body(empty_body())
                .unwrap())
        })
    }
}

impl<S> Service<Request<Body>> for GrpcWeb<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
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
            } => match self.cors.simple(req.headers()) {
                Ok(headers) => {
                    trace!(kind = "simple", path = ?req.uri().path(), ?encoding, ?accept);

                    let fut = self.inner.call(coerce_request(req, encoding));

                    Box::pin(async move {
                        let mut res = coerce_response(fut.await?, accept);
                        res.headers_mut().extend(headers);
                        Ok(res)
                    })
                }
                Err(e) => {
                    debug!(kind = "simple", error=?e, ?req);
                    self.response(StatusCode::FORBIDDEN)
                }
            },

            // The request's content-type matches one of the 4 supported grpc-web
            // content-types, but the request method is not `POST`.
            // This is not a valid grpc-web request, return HTTP 405.
            RequestKind::GrpcWeb { .. } => {
                debug!(kind = "simple", error="method not allowed", method = ?req.method());
                self.response(StatusCode::METHOD_NOT_ALLOWED)
            }

            // A valid grpc-web preflight request, regardless of HTTP version.
            // This is handled by the cors module.
            RequestKind::GrpcWebPreflight {
                origin,
                request_headers,
            } => match self.cors.preflight(req.headers(), origin, request_headers) {
                Ok(headers) => {
                    trace!(kind = "preflight", path = ?req.uri().path(), ?origin);
                    self.no_content(headers)
                }
                Err(e) => {
                    debug!(kind = "preflight", error = ?e, ?req);
                    self.response(StatusCode::FORBIDDEN)
                }
            },

            // All http/2 requests that are not grpc-web or grpc-web preflight
            // are passed through to the inner service, whatever they are.
            RequestKind::Other(Version::HTTP_2) => {
                debug!(kind = "other h2", content_type = ?req.headers().get(header::CONTENT_TYPE));
                Box::pin(self.inner.call(req))
            }

            // Return HTTP 400 for all other requests.
            RequestKind::Other(_) => {
                debug!(kind = "other h1", content_type = ?req.headers().get(header::CONTENT_TYPE));
                self.response(StatusCode::BAD_REQUEST)
            }
        }
    }
}

impl<S: NamedService> NamedService for GrpcWeb<S> {
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

        if let (&Method::OPTIONS, Some(origin), Some(value)) =
            (method, headers.get(ORIGIN), headers.get(REQUEST_HEADERS))
        {
            match value.to_str() {
                Ok(h) if h.contains("x-grpc-web") => {
                    return RequestKind::GrpcWebPreflight {
                        origin,
                        request_headers: value,
                    };
                }
                _ => {}
            }
        }

        RequestKind::Other(version)
    }
}

// Mutating request headers to conform to a gRPC request is not really
// necessary for us at this point. We could remove most of these except
// maybe for inserting `header::TE`, which tonic should check?
fn coerce_request(mut req: Request<Body>, encoding: Encoding) -> Request<Body> {
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
        .map(Body::wrap_stream)
}

fn coerce_response(res: Response<BoxBody>, encoding: Encoding) -> Response<BoxBody> {
    let mut res = res
        .map(|b| GrpcWebCall::response(b, encoding))
        .map(BoxBody::new);

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
    use http::header::{CONTENT_TYPE, ORIGIN};

    #[derive(Clone)]
    struct Svc;

    impl tower_service::Service<Request<Body>> for Svc {
        type Response = Response<BoxBody>;
        type Error = String;
        type Future = BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request<Body>) -> Self::Future {
            Box::pin(async { Ok(Response::new(BoxBody::empty())) })
        }
    }

    impl NamedService for Svc {
        const NAME: &'static str = "test";
    }

    mod grpc_web {
        use super::*;
        use http::HeaderValue;

        fn request() -> Request<Body> {
            Request::builder()
                .method(Method::POST)
                .header(CONTENT_TYPE, GRPC_WEB)
                .header(ORIGIN, "http://example.com")
                .body(Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn default_cors_config() {
            let mut svc = crate::enable(Svc);
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
        async fn origin_not_allowed() {
            let mut svc = crate::config()
                .allow_origins(vec!["http://localhost"])
                .enable(Svc);

            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::FORBIDDEN)
        }

        #[tokio::test]
        async fn only_post_allowed() {
            let mut svc = crate::enable(Svc);

            for method in &[
                Method::GET,
                Method::PUT,
                Method::DELETE,
                Method::HEAD,
                Method::OPTIONS,
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

            for ct in &[GRPC_WEB_TEXT, GRPC_WEB_PROTO, GRPC_WEB_PROTO, GRPC_WEB] {
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
        use crate::cors::{REQUEST_HEADERS, REQUEST_METHOD};
        use http::HeaderValue;

        const SUCCESS: StatusCode = StatusCode::NO_CONTENT;

        fn request() -> Request<Body> {
            Request::builder()
                .method(Method::OPTIONS)
                .header(ORIGIN, "http://example.com")
                .header(REQUEST_HEADERS, "x-grpc-web")
                .header(REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn origin_not_allowed() {
            let mut svc = crate::config()
                .allow_origins(vec!["http://foo.com"])
                .enable(Svc);

            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn missing_request_method() {
            let mut svc = crate::enable(Svc);

            let mut req = request();
            req.headers_mut().remove(REQUEST_METHOD);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::FORBIDDEN);
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
                req.headers_mut().insert(
                    REQUEST_METHOD,
                    HeaderValue::from_maybe_shared(method.to_string()).unwrap(),
                );

                let res = svc.call(req).await.unwrap();

                assert_eq!(
                    res.status(),
                    StatusCode::FORBIDDEN,
                    "{} should not be allowed",
                    method
                );
            }
        }

        #[tokio::test]
        async fn h1_missing_origin_is_err() {
            let mut svc = crate::enable(Svc);
            let mut req = request();
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        #[tokio::test]
        async fn h2_missing_origin_is_ok() {
            let mut svc = crate::enable(Svc);

            let mut req = request();
            *req.version_mut() = Version::HTTP_2;
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn h1_missing_x_grpc_web_header_is_err() {
            let mut svc = crate::enable(Svc);

            let mut req = request();
            req.headers_mut().remove(REQUEST_HEADERS);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        #[tokio::test]
        async fn h2_missing_x_grpc_web_header_is_ok() {
            let mut svc = crate::enable(Svc);

            let mut req = request();
            *req.version_mut() = Version::HTTP_2;
            req.headers_mut().remove(REQUEST_HEADERS);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn valid_grpc_web_preflight() {
            let mut svc = crate::enable(Svc);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), SUCCESS);
        }
    }

    mod grpc {
        use super::*;
        use http::HeaderValue;

        fn request() -> Request<Body> {
            Request::builder()
                .version(Version::HTTP_2)
                .header(CONTENT_TYPE, GRPC)
                .body(Body::empty())
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
                .body(Body::empty())
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

        fn request() -> Request<Body> {
            Request::builder()
                .header(CONTENT_TYPE, "application/text")
                .body(Body::empty())
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
