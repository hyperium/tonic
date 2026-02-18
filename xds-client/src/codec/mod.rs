//! Codec for encoding/decoding xDS messages.
//!
//! The codec layer converts between crate-owned message types
//! ([`DiscoveryRequest`], [`DiscoveryResponse`]) and serialized bytes.
//! This abstraction allows different protobuf implementations
//! (prost, google-protobuf) to be used with the same xDS client logic.

use crate::error::Result;
use crate::message::{DiscoveryRequest, DiscoveryResponse};
use bytes::Bytes;

#[cfg(feature = "codegen-prost")]
pub mod prost;

/// Trait for encoding/decoding xDS discovery messages.
///
/// Implementations convert between the crate-owned message types
/// and their serialized wire format.
pub trait XdsCodec: Send + Sync + 'static {
    /// Encode a [`DiscoveryRequest`] to bytes.
    fn encode_request(&self, request: &DiscoveryRequest<'_>) -> Result<Bytes>;

    /// Decode bytes into a [`DiscoveryResponse`].
    fn decode_response(&self, bytes: Bytes) -> Result<DiscoveryResponse>;
}
