/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! The official Rust implementation of [gRPC], a high performance, open source,
//! universal RPC framework.
//!
//! > NOTE: This version is a preview and not recommended for any production
//! > use.  All APIs are unstable.  Proceed at your own risk.
//!
//! # Documentation, Examples, and Getting Started
//!
//! Please see [our website] for everything you should need to get started using
//! gRPC.
//!
//! # Feature Flags
//!
//! The only currently-supported feature flags are the defaults.
//!
//! # Modules
//!
//! * [`client`] - Creating and working with gRPC client-side channels
//! * [`credentials`] - Securing connections and providing access tokens
//! * [`metadata`] - Data sent with all RPCs typically used by interceptors
//! * [`core`] - Common types shared between clients and servers
//! * [`attributes`] - Generic key/value storage used by gRPC plugins
//!
//! [gRPC]: https://grpc.io
//! [our website]: https://grpc.io/docs/languages/rust
#![allow(dead_code, unused_variables)]

pub mod attributes;
pub mod client;
pub(crate) mod codec;
pub mod core;
pub mod credentials;
pub mod metadata;
pub mod server;

mod byte_str;
mod inmemory;
mod macros;
mod rt;
mod send_future;
mod status;

pub use status::Result;
pub use status::StatusCodeError;
pub use status::StatusError;
/// A re-export of [`async-trait`](https://docs.rs/async-trait) for use with
/// codegen.
pub use tonic::async_trait;

#[cfg(feature = "__unstable")]
#[doc(hidden)]
pub mod __unstable {
    pub use crate::private::Internal;

    pub mod rt {
        pub use crate::rt::*;
    }
    pub mod client {
        pub mod load_balancing {
            pub use crate::client::load_balancing::*;
        }
        pub mod name_resolution {
            pub use crate::client::name_resolution::*;
        }
        pub mod service_config {
            pub use crate::client::service_config::*;
        }
    }
    pub mod credentials {
        pub mod client {
            pub use crate::credentials::client::*;
        }
        pub mod server {
            pub use crate::credentials::server::*;
        }
        pub mod common {
            pub use crate::credentials::common::*;
        }
    }
}

mod private {
    /// A zero-sized type used to seal methods on a public trait.
    ///
    /// Because this type is private to this crate, it cannot be constructed or
    /// named by external crates. As a result, any method requiring an `Internal`
    /// argument becomes uncallable from outside the crate.
    pub struct Internal;
}

#[cfg(test)]
mod echo_pb {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/generated/grpc_examples_echo.rs"
    ));
}
