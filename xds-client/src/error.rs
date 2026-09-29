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

//! Error types for the xDS client.

use thiserror::Error;

/// Error type for the xDS client.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Failed to connect to the xDS server.
    #[error("failed to connect: {0}")]
    Connection(String),

    /// Error on the ADS stream.
    #[cfg(feature = "transport-tonic")]
    #[error("stream error: {0}")]
    Stream(#[from] tonic::Status),

    /// Call credentials failed, or require a secure transport.
    #[error("call credentials error: {0}")]
    CallCredentials(String),

    /// TLS configuration could not be constructed for the xDS server.
    #[cfg(feature = "transport-tonic")]
    #[error("TLS configuration error: {0}")]
    TlsConfig(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// The stream was closed unexpectedly.
    #[error("stream closed unexpectedly")]
    StreamClosed,

    /// Failed to decode a protobuf message.
    #[cfg(feature = "codegen-prost")]
    #[error("decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    /// Resource validation failed.
    #[error("resource validation failed: {0}")]
    Validation(String),

    /// Resource does not exist.
    ///
    /// This indicates the resource has been deleted or was never created.
    #[error("resource does not exist")]
    ResourceDoesNotExist,
}

/// Result type alias for xDS client operations.
pub type Result<T> = std::result::Result<T, Error>;
