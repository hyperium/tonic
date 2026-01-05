//! Provides abstraction for transport layers.

use crate::error::Result;
use std::future::Future;

#[cfg(feature = "transport-tonic")]
pub mod tonic;

/// A discovery request to send to the xDS server.
#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    // TODO: Add fields as needed
}

/// A discovery response from the xDS server.
#[derive(Debug, Clone)]
pub struct DiscoveryResponse {
    // TODO: Add fields as needed
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
    /// This may be called multiple times for reconnection.
    fn new_stream(&self) -> impl Future<Output = Result<Self::Stream>> + Send;
}

/// A bidirectional stream for xDS ADS communication.
pub trait TransportStream: Send + 'static {
    /// Send a discovery request to the server.
    fn send(&mut self, request: DiscoveryRequest) -> impl Future<Output = Result<()>> + Send;

    /// Receive the next discovery response from the server.
    ///
    /// Returns:
    /// - `Ok(Some(response))` - Received a response.
    /// - `Ok(None)` - Stream closed normally.
    /// - `Err(_)` - Stream error (connection dropped, etc.)
    fn recv(&mut self) -> impl Future<Output = Result<Option<DiscoveryResponse>>> + Send;
}
