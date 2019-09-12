#![recursion_limit = "256"]
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(html_logo_url = "file:///Users/lucio/Downloads/tonic_bubbles_with_word_bigger.svg")]
#![doc(html_root_url = "https://docs.rs/tonic/0.1.0")]
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
pub mod metadata;
pub mod server;

#[cfg(feature = "transport")]
pub mod transport;

mod request;
mod response;
mod status;

pub use async_trait::async_trait;
#[doc(inline, hidden)]
pub use body::BoxBody;
#[doc(inline)]
pub use codec::Streaming;
pub use request::Request;
pub use response::Response;
pub use status::{Code, Status};
#[doc(inline)]
pub use transport::{Channel, Server};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

#[doc(hidden)]
pub mod codegen;
