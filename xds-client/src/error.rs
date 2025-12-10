//! Error types for the xDS client.

use thiserror::Error;

/// Error type for the xDS client.
#[derive(Debug, Error)]
pub enum Error {
    // TODO: Add error variants as needed
}

/// Result type alias for xDS client operations.
pub type Result<T> = std::result::Result<T, Error>;