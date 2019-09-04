#![recursion_limit = "512"]
#![warn(missing_debug_implementations)]

//! gRPC implementation

pub mod body;
pub mod client;
pub mod codec;
#[doc(hidden)]
pub mod error;
pub mod metadata;
pub mod server;

#[cfg(feature = "transport")]
pub mod transport;

mod request;
mod response;
mod status;

#[doc(inline, hidden)]
pub use body::BoxBody;
pub use request::Request;
pub use response::Response;
pub use status::{Code, Status};
pub use tonic_macros::{client, server};

pub(crate) use error::Error;

#[doc(hidden)]
pub mod _codegen {
    pub use futures_core::Stream;
    pub use futures_util::future::{ok, poll_fn, Ready};
    pub use http_body::Body as HttpBody;
    pub use std::future::Future;
    pub use std::pin::Pin;
    pub use std::task::{Context, Poll};
    pub use tower_service::Service;

    #[cfg(feature = "transport")]
    pub use hyper::Body as HyperBody;

    pub type BoxFuture<T, E> =
        self::Pin<Box<dyn self::Future<Output = Result<T, E>> + Send + 'static>>;
    pub type BoxStream<T> =
        self::Pin<Box<dyn futures_core::Stream<Item = Result<T, crate::Status>> + Send + 'static>>;

    pub mod http {
        pub use http::*;
    }
}
