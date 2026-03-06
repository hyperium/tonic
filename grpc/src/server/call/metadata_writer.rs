use std::marker::Send;

use crate::server::call::Metadata;
use crate::Status;

/// A trait for writing initial metadata.
#[trait_variant::make(Send)]
pub trait InitialMetadataWriter: Send {
    /// Sends initial metadata.
    async fn send_initial_metadata(self, metadata: Metadata) -> Result<(), Status>;
}

/// A trait for writing trailing metadata.
#[trait_variant::make(Send)]
pub trait TrailingMetadataWriter: Send {
    /// Sends trailing metadata.
    async fn send_trailing_metadata(self, metadata: Metadata) -> Result<(), Status>;
}
