#![recursion_limit = "512"]
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]

//! A rust implementation of [gRPC], a high performance, open source, general
//! RPC framework that puts mobile and HTTP/2 first.
//!
//! [tonic] is a gRPC over HTTP2 implementation focused on **high
//! performance**, **interoperability**, and **flexibility**. This library was
//! created to have first class support of async/await.
//!
//! # Examples
//!
//! Examples can be found in the [`tonic-examples`] crate.
//!
//! # Generic implementation
//!
//! TODO: write generic implementation docs
//!
//! # Transport
//!
//! TODO: write transport docs
//!
//! [gRPC]: https://grpc.io
//! [tonic]: https://github.com/hyperium/tonic
//! [`tonic-examples`]: https://github.com/hyperium/tonic/tree/master/tonic-examples/src

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
#[doc(inline)]
pub use transport::{Channel, Server};

pub(crate) use error::Error;

#[doc(hidden)]
pub use async_trait::async_trait as server_trait;

#[doc(hidden)]
pub mod _codegen {
    pub use async_trait::async_trait;
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
