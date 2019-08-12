#![feature(async_await, type_alias_impl_trait)]

//! gRPC implementation

#[doc(hidden)]
pub mod error;
pub mod metadata;

mod body;
mod codec;
mod request;
mod response;
mod server;
mod status;

pub use request::Request;
pub use response::Response;
pub use status::{Code, Status};
pub use tonic_macros::server;

use std::future::Future;
use std::sync::Arc;

pub trait GrpcInnerService<Request> {
    type Response;
    type Future: Future<Output = Result<Self::Response, Status>>;

    fn call(self: Arc<Self>, request: Request) -> Self::Future;
}

#[doc(hidden)]

pub mod _codegen {
    pub use futures_util::future::{ok, Ready};
    pub use std::future::Future;
    pub use std::pin::Pin;
    pub use std::task::{Context, Poll};
    pub use tower_service::Service;
    pub type ResponseFuture<T> =
        self::Pin<Box<dyn self::Future<Output = Result<T, crate::Status>> + Send + 'static>>;

    pub mod http {
        pub use http::*;
    }
}
