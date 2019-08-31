// TODO: make this private again
pub mod add_origin;

pub use self::add_origin::AddOrigin;

use crate::body::Body;
use http::{Request, Response};
use http_body::Body as HttpBody;
use std::future::Future;
use std::task::{Context, Poll};
use tower_service::Service;

pub trait GrpcService<ReqBody> {
    type ResponseBody: Body + HttpBody;
    type Error: Into<crate::Error>;

    type Future: Future<Output = Result<Response<Self::ResponseBody>, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future;
}

impl<T, ReqBody, ResBody> GrpcService<ReqBody> for T
where
    T: Service<Request<ReqBody>, Response = Response<ResBody>>,
    T::Error: Into<crate::Error>,
    ResBody: Body + HttpBody,
    <ResBody as HttpBody>::Error: Into<crate::Error>,
{
    type ResponseBody = ResBody;
    type Error = T::Error;
    type Future = T::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::poll_ready(self, cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        Service::call(self, request)
    }
}
