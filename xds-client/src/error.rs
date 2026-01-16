//! Error types for the xDS client.

use thiserror::Error;

/// Error type for the xDS client.
#[derive(Debug, Error)]
pub enum Error {
    /// Failed to connect to the xDS server.
    #[error("failed to connect: {0}")]
    Connection(String),

    /// Error on the ADS stream.
    #[cfg(feature = "transport-tonic")]
    #[error("stream error: {0}")]
    Stream(#[from] tonic::Status),

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
}

/// Result type alias for xDS client operations.
pub type Result<T> = std::result::Result<T, Error>;
