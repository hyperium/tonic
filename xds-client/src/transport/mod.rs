//! Provides abstraction for transport layers.

use crate::error::Result;
use bytes::Bytes;
use std::future::Future;

#[cfg(feature = "transport-tonic")]
pub mod tonic;

mod sealed {
    pub trait Sealed {}
}

/// Factory for creating xDS transport streams.
///
/// This abstraction allows for different transport implementations:
/// - Tonic-based gRPC transport
/// - The upcoming gRPC Rust transport
/// - Mock transport for testing
/// - Other custom transports
pub trait Transport: Send + Sync + 'static {
    /// The stream type produced by this transport.
    type Stream: TransportStream;

    /// Creates a new bidirectional ADS stream to the xDS server.
    ///
    /// # Arguments
    ///
    /// * `initial_requests` - Requests to send immediately when establishing the stream.
    ///   This is critical for xDS servers that don't send response headers until
    ///   they receive the first request (prevents deadlock).
    ///
    /// This may be called multiple times for reconnection.
    fn new_stream(
        &self,
        initial_requests: Vec<Bytes>,
    ) -> impl Future<Output = Result<Self::Stream>> + Send;
}

/// A bidirectional byte stream for xDS ADS communication.
///
/// Raw byte transport where the bytes are serialized DiscoveryRequest/DiscoveryResponse
/// (de)serialization is handled at the xDS client worker layer.
// Sealed for now to limit API surface.
pub trait TransportStream: sealed::Sealed + Send + 'static {
    /// Send serialized DiscoveryRequest bytes to the server.
    fn send(&mut self, request: Bytes) -> impl Future<Output = Result<()>> + Send;

    /// Receive serialized DiscoveryResponse bytes from the server.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` - Received a response.
    /// - `Ok(None)` - Stream closed normally.
    /// - `Err(_)` - Stream error (connection dropped, etc.)
    fn recv(&mut self) -> impl Future<Output = Result<Option<Bytes>>> + Send;
}

#[cfg(feature = "transport-tonic")]
impl sealed::Sealed for tonic::TonicAdsStream {}
