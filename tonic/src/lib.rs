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

//! A Rust implementation of [gRPC], a high performance, open source, general
//! RPC framework that puts mobile and HTTP/2 first.
//!
//! [`tonic`] is a gRPC over HTTP/2 implementation focused on **high
//! performance**, **interoperability**, and **flexibility**. This library was
//! created to have first class support of async/await and to act as a core building
//! block for production systems written in Rust.
//!
//! # Examples
//!
//! Examples can be found in the [`tonic-examples`] crate.
//!
//! # Getting Started
//!
//! Follow the instructions in the [`tonic-build`] crate documentation.
//!
//! # Feature Flags
//!
//! - `transport`: Enables the fully featured, batteries included client and server
//! implementation based on [`hyper`], [`tower`] and [`tokio`]. Enabled by default.
//! - `codegen`: Enables all the required exports and optional dependencies required
//! for [`tonic-build`]. Enabled by default.
//! - `openssl`: Enables the `openssl` based tls options for the `transport` feature`. Not
//! enabled by default.
//! - `rustls`: Enables the `ruslts` based tls options for the `transport` feature`. Not
//! enabled by default.
//! - `prost`: Enables the [`prost`] based gRPC [`Codec`] implementation.
//!
//! # Structure
//!
//! ## Generic implementation
//!
//! The main goal of [`tonic`] is to provide a generic gRPC implementation over HTTP/2
//! framing. This means at the lowest level this library provides the ability to use
//! a generic HTTP/2 implementation with different types of gRPC encodings formats. Generally,
//! some form of codegen should be used instead of interacting directly with the items in
//! [`client`] and [`server`].
//!
//! ## Transport
//!
//! The [`transport`] module contains a fully featured HTTP/2.0 [`Channel`] (gRPC terminology)
//! and [`Server`]. These implementations are built on top of [`tokio`], [`hyper`] and [`tower`].
//! It also provides many of the features that the core gRPC libraries provide such as load balancing,
//! tls, timeouts, and many more. This implementation can also be used as a reference implementation
//! to build even more feature rich clients and servers. This module also provides the ability to choose
//! between [`rustls`] and [`openssl`] for the tls backend.
//!
//! [gRPC]: https://grpc.io
//! [`tonic`]: https://github.com/hyperium/tonic
//! [`tonic-build`]: https://docs.rs/tonic-build
//! [`tonic-examples`]: https://github.com/hyperium/tonic/tree/master/tonic-examples/src
//! [`Codec`]: codec/trait.Codec.html
//! [`Channel`]: transport/struct.Channel.html
//! [`Server`]: transport/struct.Server.html
//! [`rustls`]: https://docs.rs/rustls
//! [`openssl`]: https://www.openssl.org

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

/// A re-export of [`async-trait`](https://docs.rs/async-trait) for use with codegen.
#[cfg(feature = "codegen")]
pub use async_trait::async_trait;

#[doc(inline)]
pub use codec::Streaming;
pub use request::Request;
pub use response::Response;
pub use status::{Code, Status};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

#[doc(hidden)]
#[cfg(feature = "codegen")]
pub mod codegen;

mod macros;
