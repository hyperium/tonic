pub use tower_grpc::*;

use std::future::Future;
use std::pin::Pin;

pub type ResponseFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Status>> + Send + 'a>>;

pub trait GrpcInnerService<Request> {
    type Response;

    fn call<'a>(&'a mut self, request: Request) -> ResponseFuture<'a, Self::Response>
    where
        Self: 'a;
}
