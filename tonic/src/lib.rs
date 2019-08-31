#![recursion_limit = "512"]

//! gRPC implementation

pub mod body;
pub mod client;
pub mod codec;
#[doc(hidden)]
pub mod error;
pub mod metadata;
pub mod server;
pub mod service;

#[cfg(feature = "transport")]
pub mod transport;

mod request;
mod response;
mod status;

pub use body::BoxBody;
pub use request::Request;
pub use response::Response;
pub use service::GrpcService;
pub use status::{Code, Status};
pub use tonic_macros::{client, server};

pub(crate) use error::Error;

use std::future::Future;
use std::sync::Arc;

pub trait GrpcInnerService<Request> {
    type Response;
    type Future: Future<Output = Result<Self::Response, Status>>;

    fn call(self: Arc<Self>, request: Request) -> Self::Future;
}

#[doc(hidden)]

pub mod _codegen {
    pub use futures_core::Stream;
    pub use futures_util::future::{ok, poll_fn, Ready};
    pub use http_body::Body as HttpBody;
    pub use std::future::Future;
    pub use std::pin::Pin;
    pub use std::task::{Context, Poll};
    pub use tower_service::Service;

    pub type BoxFuture<T, E> =
        self::Pin<Box<dyn self::Future<Output = Result<T, E>> + Send + 'static>>;
    pub type BoxStream<T> =
        self::Pin<Box<dyn futures_core::Stream<Item = Result<T, crate::Status>> + Send + 'static>>;

    pub mod http {
        pub use http::*;
    }
}
