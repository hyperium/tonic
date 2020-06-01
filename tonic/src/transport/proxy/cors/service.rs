use super::{Config, CorsResource};

use futures_core::future::Future;

use crate::body::BoxBody;
use http::{self, Request, Response, StatusCode};
use std::pin::Pin;
use tower_service::Service;

use hyper::Body;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Decorates a service, providing an implementation of the CORS specification.
#[derive(Debug, Clone)]
pub struct CorsService<S> {
    #[doc(hidden)]
    pub inner: S,
    config: Arc<Config>,
}

#[doc(hidden)]
impl<S> CorsService<S> {
    pub fn new(inner: S, config: Arc<Config>) -> CorsService<S> {
        CorsService { inner, config }
    }
}

impl<S> Service<Request<Body>> for CorsService<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send + Clone,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + 'static,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    //type Future = MapErr<Instrumented<S::Future>, fn(S::Error) -> crate::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let state = self.config.process_request(&req);
        let version = req.version();

        let response_future = self.inner.call(req);

        let fut = async move {
            //If it's a HTTP/2 request, let it through.
            if version == http::Version::HTTP_2 {
                let response = response_future.await.ok().unwrap();
                return Ok(response);
            }
            match state {
                Ok(CorsResource::Preflight(headers)) => {
                    let mut response = http::Response::new(BoxBody::empty());
                    *response.status_mut() = StatusCode::NO_CONTENT;
                    *response.headers_mut() = headers;
                    Ok(response)
                }
                Ok(CorsResource::Simple(headers)) => {
                    let mut response = response_future.await.ok().unwrap();
                    //let mut response = http::Response::new(BoxBody::empty());
                    response.headers_mut().extend(headers);
                    Ok(response)
                }
                Err(_) => {
                    let mut response = http::Response::new(BoxBody::empty());
                    *response.status_mut() = StatusCode::FORBIDDEN;
                    Ok(response)
                }
            }
        };
        Box::pin(fut)
    }
}
