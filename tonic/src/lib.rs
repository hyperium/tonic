pub use tower_grpc::*;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ResponseFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Status>> + Send + 'a>>;

pub trait GrpcInnerService<Request> {
    type Response;
    type Future: Future<Output = Result<Self::Response, Status>>;

    fn call(self: Arc<Self>, request: Request) -> Self::Future;
}
