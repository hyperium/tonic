use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::header::{self, HeaderName};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use hyper::Body;
use tonic::body::BoxBody;
use tonic::transport::NamedService;
use tower_service::Service;

use crate::call::{coerce_request, coerce_response, is_grpc_web, is_grpc_web_preflight, Encoding};
use crate::cors::{AllowedOrigins, Builder, Cors, CorsResource};

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
        if !(is_grpc_web(req.headers()) || is_grpc_web_preflight(&req)) {
            return Box::pin(self.inner.call(req));
        }

        match self.cors.process_request(&req) {
            Ok(CorsResource::Simple(headers)) => {
                let response_encoding = Encoding::from_accept(req.headers());
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
                Box::pin(async { Ok(res) })
            }
            Err(_) => Box::pin(async { Ok(http_response(StatusCode::FORBIDDEN)) }),
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
