use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::future::{self, Future};
use http::header::{self, HeaderName};
use http::{Method, Request, Response, StatusCode};
use hyper::Body;
use tonic::body::BoxBody;
use tonic::transport::NamedService;
use tower_service::Service;

use crate::call::{
    self, coerce_request, coerce_response, is_grpc_web, is_grpc_web_preflight, Encoding,
};
use crate::cors::{AllowedOrigins, Config, CorsBuilder, CorsResource};

type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'static>>;

#[derive(Debug, Clone)]
pub struct GrpcWeb<S> {
    inner: S,
    cors: Config,
}

impl<S> GrpcWeb<S> {
    // TODO: Expose a builder to configure CORS
    pub fn new(inner: S) -> Self {
        let allowed_headers = &[
            header::USER_AGENT,
            header::CACHE_CONTROL,
            header::CONTENT_TYPE,
            HeaderName::from_static("keep-alive"),
            HeaderName::from_static("x-grpc-web"),
            HeaderName::from_static("x-user-agent"),
            HeaderName::from_static("grpc-timeout"),
            // TODO: remove from default allowed headers
            HeaderName::from_static("custom-header-1"),
        ];

        let exposed_headers = &[
            HeaderName::from_static("grpc-status"),
            HeaderName::from_static("grpc-message"),
        ];

        let allowed_methods = &[Method::POST, Method::OPTIONS];

        let max_age = std::time::Duration::from_secs(24 * 60 * 60);

        let allowed_origins = AllowedOrigins::Any { allow_null: false };

        let cors = CorsBuilder::new()
            .allow_origins(allowed_origins)
            .allow_methods(allowed_methods)
            .allow_headers(allowed_headers)
            .expose_headers(exposed_headers)
            .max_age(max_age)
            .into_config();

        GrpcWeb { inner, cors }
    }
}

impl<S> Service<Request<Body>> for GrpcWeb<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + Send,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        if !is_grpc_web(&req) && !is_grpc_web_preflight(&req) {
            return Box::pin(self.inner.call(req));
        }

        match self.cors.process_request(&req) {
            Ok(CorsResource::Simple(headers)) => {
                let response_encoding = Encoding::from(call::accept(&req));
                let request_future = self.inner.call(coerce_request(req));

                Box::pin(async move {
                    let mut res = coerce_response(request_future.await?, response_encoding);
                    res.headers_mut().extend(headers);
                    Ok(res)
                })
            }
            Ok(CorsResource::Preflight(headers)) => {
                let mut res = http_response(StatusCode::NO_CONTENT);
                res.headers_mut().extend(headers);
                Box::pin(future::ok(res))
            }
            Err(_) => Box::pin(future::ok(http_response(StatusCode::FORBIDDEN))),
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
