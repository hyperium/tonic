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
//! - `tls`: Enables the `rustls` based TLS options for the `transport` feature`. Not
//! enabled by default.
//! - `tls-roots`: Adds system trust roots to `rustls`-based gRPC clients using the
//! `rustls-native-certs` crate. Not enabled by default. `tls` must be enabled to use
//! `tls-roots`.
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
//! to build even more feature rich clients and servers. This module also provides the ability to
//! enable TLS using [`rustls`], via the `tls` feature flag.
//!
//! [gRPC]: https://grpc.io
//! [`tonic`]: https://github.com/hyperium/tonic
//! [`tokio`]: https://docs.rs/tokio
//! [`prost`]: https://docs.rs/prost
//! [`hyper`]: https://docs.rs/hyper
//! [`tower`]: https://docs.rs/tower
//! [`tonic-build`]: https://docs.rs/tonic-build
//! [`tonic-examples`]: https://github.com/hyperium/tonic/tree/master/examples
//! [`Codec`]: codec/trait.Codec.html
//! [`Channel`]: transport/struct.Channel.html
//! [`Server`]: transport/struct.Server.html
//! [`rustls`]: https://docs.rs/rustls
//! [`client`]: client/index.html
//! [`transport`]: transport/index.html

#![recursion_limit = "256"]
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://github.com/hyperium/tonic/raw/master/.github/assets/tonic-docs.png"
)]
#![doc(html_root_url = "https://docs.rs/tonic/0.2.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]
#![doc(test(no_crate_inject, attr(deny(rust_2018_idioms))))]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod body;
pub mod client;
pub mod codec;
pub mod metadata;
pub mod server;

#[cfg(feature = "transport")]
#[cfg_attr(docsrs, doc(cfg(feature = "transport")))]
pub mod transport;

mod interceptor;
mod macros;
mod request;
mod response;
mod status;

/// A re-export of [`async-trait`](https://docs.rs/async-trait) for use with codegen.
#[cfg(feature = "codegen")]
#[cfg_attr(docsrs, doc(cfg(feature = "codegen")))]
pub use async_trait::async_trait;

#[doc(inline)]
pub use codec::Streaming;
pub use interceptor::Interceptor;
pub use request::{IntoRequest, IntoStreamingRequest, Request};
pub use response::Response;
pub use status::{Code, Status};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

#[doc(hidden)]
#[cfg(feature = "codegen")]
#[cfg_attr(docsrs, doc(cfg(feature = "codegen")))]
pub mod codegen;
