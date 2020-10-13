use crate::{
    call::{classify_request, coerce_request, coerce_response, Encoding, RequestKind},
    cors::Cors,
};
use http::{Method, Request, Response, StatusCode, Version};
use hyper::Body;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tonic::{body::BoxBody, transport::NamedService};
use tower_service::Service;

type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>>;
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone)]
pub struct GrpcWeb<S> {
    inner: S,
    cors: Cors,
}

impl<S> GrpcWeb<S> {
    pub fn with_cors(inner: S, cors: Cors) -> Self {
        GrpcWeb { inner, cors }
    }

    pub fn new(inner: S) -> Self {
        GrpcWeb {
            inner,
            cors: Cors::default(),
        }
    }
}

impl<S> Service<Request<Body>> for GrpcWeb<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Error> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        use RequestKind::*;

        match classify_request(req.headers(), req.method(), req.version()) {
            GrpcWeb(&Method::POST) => {
                let headers = match self.cors.check_simple(req.headers()) {
                    Ok(headers) => headers,
                    Err(_) => return Box::pin(async { Ok(http_response(StatusCode::FORBIDDEN)) }),
                };

                let encoding = Encoding::from_content_type(req.headers());

                let response_encoding = Encoding::from_accept(req.headers());

                let request_future = self.inner.call(coerce_request(req, encoding));

                Box::pin(async move {
                    let mut res = coerce_response(request_future.await?, response_encoding);
                    res.headers_mut().extend(headers);
                    Ok(res)
                })
            }

            GrpcWeb(_) => Box::pin(async { Ok(http_response(StatusCode::METHOD_NOT_ALLOWED)) }),

            GrpcWebPreflight {
                origin,
                request_headers,
            } => {
                let headers =
                    match self
                        .cors
                        .check_preflight(req.headers(), origin, request_headers)
                    {
                        Ok(headers) => headers,
                        Err(error) => {
                            println!("log debug this: {:?}", error);
                            return Box::pin(async { Ok(error.into_response()) });
                        }
                    };

                let mut res = http_response(StatusCode::NO_CONTENT);
                res.headers_mut().extend(headers);
                Box::pin(async { Ok(res) })
            }
            Other(Version::HTTP_2) => Box::pin(self.inner.call(req)),
            Other(_) => Box::pin(async { Ok(http_response(StatusCode::BAD_REQUEST)) }),
        }
    }
}

fn http_response(status: StatusCode) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(BoxBody::empty())
        .unwrap()
}

impl<S: NamedService> NamedService for GrpcWeb<S> {
    const NAME: &'static str = S::NAME;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_types::*;
    use http::header::{CONTENT_TYPE, ORIGIN};

    struct Svc;

    type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

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

    fn service(cors: Cors) -> GrpcWeb<Svc> {
        GrpcWeb::with_cors(Svc, cors)
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
            let mut svc = service(Cors::default());
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn with_cors_disabled() {
            let mut svc = service(Cors::disabled());
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn without_origin() {
            let mut svc = service(Cors::default());

            let mut req = request();
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn invalid_origin() {
            let cors = Cors::builder().allow_origin("http://localhost").build();
            let mut svc = GrpcWeb::with_cors(Svc, cors);

            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::FORBIDDEN)
        }

        #[tokio::test]
        async fn only_post_allowed() {
            let mut svc = service(Cors::default());

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
            let mut svc = service(Cors::default());

            for ct in &[GRPC_WEB_TEXT, GRPC_WEB_PROTO, GRPC_WEB_PROTO, GRPC_WEB] {
                let mut req = request();
                req.headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static(ct));

                let res = svc.call(req).await.unwrap();

                assert_eq!(res.status(), StatusCode::OK);
            }
            //
        }
    }

    mod options {
        use super::*;
        use crate::cors_headers::{REQUEST_HEADERS, REQUEST_METHOD};
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
            let cors = Cors::builder().allow_origin("http://foo.com").build();
            let mut svc = service(cors);
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn missing_request_method() {
            let mut svc = service(Cors::default());

            let mut req = request();
            req.headers_mut().remove(REQUEST_METHOD);

            let res = svc.call(req).await.unwrap();

            //  TODO: this returns forbidden BUT, LOG, DEBUG, TRACE the reason
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }

        #[tokio::test]
        async fn only_post_and_options_allowed() {
            let mut svc = service(Cors::default());

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
            let mut svc = service(Cors::default());
            let mut req = request();
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        #[tokio::test]
        async fn h2_missing_origin_is_ok() {
            let mut svc = service(Cors::default());

            let mut req = request();
            *req.version_mut() = Version::HTTP_2;
            req.headers_mut().remove(ORIGIN);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn h1_missing_x_grpc_web_header_is_err() {
            let mut svc = service(Cors::default());

            let mut req = request();
            req.headers_mut().remove(REQUEST_HEADERS);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        }

        #[tokio::test]
        async fn h2_missing_x_grpc_web_header_is_ok() {
            let mut svc = service(Cors::default());

            let mut req = request();
            *req.version_mut() = Version::HTTP_2;
            req.headers_mut().remove(REQUEST_HEADERS);

            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn valid_grpc_web_preflight() {
            let mut svc = service(Cors::default());
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
            let mut svc = service(Cors::default());

            let req = request();
            let res = svc.call(req).await.unwrap();

            assert_eq!(res.status(), StatusCode::OK)
        }

        #[tokio::test]
        async fn h1_is_err() {
            let mut svc = service(Cors::default());

            let req = Request::builder()
                .header(CONTENT_TYPE, GRPC)
                .body(Body::empty())
                .unwrap();

            let res = svc.call(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST)
        }

        #[tokio::test]
        async fn content_type_variants() {
            let mut svc = service(Cors::default());

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
            let mut svc = service(Cors::default());
            let res = svc.call(request()).await.unwrap();

            assert_eq!(res.status(), StatusCode::BAD_REQUEST)
        }

        #[tokio::test]
        async fn h2_is_ok() {
            let mut svc = service(Cors::default());
            let mut req = request();
            *req.version_mut() = Version::HTTP_2;

            let res = svc.call(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK)
        }
    }
}
