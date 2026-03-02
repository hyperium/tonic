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
//! universal RPC framework
//!
//! This version is in progress and not recommended for any production use.  All
//! APIs are unstable.  Proceed at your own risk.
//!
//! [gRPC]: https://grpc.io
#![allow(dead_code, unused_variables)]

pub mod client;
pub mod core;
pub mod credentials;
pub mod inmemory;
mod macros;
mod status;
pub use status::ServerStatus;
pub use status::Status;
pub use status::StatusCode;
mod attributes;
mod byte_str;
mod codec;
mod rt;
mod send_future;
pub mod server;
pub mod service;
#[cfg(test)]
mod echo_pb {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/generated/grpc_examples_echo.rs"
    ));
}
