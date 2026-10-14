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

//! Provides abstraction for transport layers.

use crate::client::config::ServerConfig;
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
    /// The sending half produced by this transport.
    type Sender: TransportSender;
    /// The receiving half produced by this transport.
    type Receiver: TransportReceiver;

    /// Creates a new bidirectional ADS stream to the xDS server,
    /// returning separate sending and receiving halves.
    ///
    /// This may be called multiple times for reconnection.
    fn new_stream(&self) -> impl Future<Output = Result<(Self::Sender, Self::Receiver)>> + Send;
}

/// Sending half of a bidirectional xDS ADS stream.
///
/// Sealed for now to limit API surface.
pub trait TransportSender: sealed::Sealed + Send + 'static {
    /// Send serialized DiscoveryRequest bytes to the server.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the TransportSender are undefined and data may be lost.
    fn send(&mut self, request: Bytes) -> impl Future<Output = Result<()>> + Send;
}

/// Receiving half of a bidirectional xDS ADS stream.
///
/// Sealed for now to limit API surface.
pub trait TransportReceiver: sealed::Sealed + Send + 'static {
    /// Receive serialized DiscoveryResponse bytes from the server.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` - Received a response.
    /// - `Ok(None)` - Stream closed normally.
    /// - `Err(_)` - Stream error (connection dropped, etc.)
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the TransportReceiver are undefined and data may be lost.
    fn recv(&mut self) -> impl Future<Output = Result<Option<Bytes>>> + Send;
}

#[cfg(feature = "transport-tonic")]
impl sealed::Sealed for tonic::TonicAdsSender {}
#[cfg(feature = "transport-tonic")]
impl sealed::Sealed for tonic::TonicAdsReceiver {}

/// Factory for creating transports to xDS servers.
///
/// This abstraction allows the client to create transports on-demand,
/// enabling features like:
/// - Server fallback (gRFC A71): Try backup servers when primary fails
/// - Connection pooling: Reuse connections to the same server
///
/// Implementations may hold configuration (e.g., TLS settings) that applies
/// to all servers.
///
/// # Example
///
/// ```ignore
/// use xds_client::{ServerConfig, TransportBuilder};
///
/// struct MyTransportBuilder { /* ... */ }
///
/// impl TransportBuilder for MyTransportBuilder {
///     type Transport = MyTransport;
///
///     async fn build(&self, server: &ServerConfig) -> Result<Self::Transport> {
///         // Create transport connected to server.uri()
///     }
/// }
/// ```
pub trait TransportBuilder: Send + Sync + 'static {
    /// The transport type produced by this builder.
    type Transport: Transport;

    /// Build a transport connected to the given server.
    ///
    /// This may be called multiple times for reconnection or fallback.
    /// Implementations may cache/pool connections internally.
    fn build(&self, server: &ServerConfig) -> impl Future<Output = Result<Self::Transport>> + Send;

    // Future extensions:
    // - `fn close(&self, server: &ServerConfig)` for explicit connection cleanup
    // - Metrics/observability hooks
}

/// In-crate mock transport for worker tests.
///
/// Lives here because [`Transport`], [`TransportSender`], and [`TransportReceiver`]
/// are sealed: test code outside this module cannot implement them.
#[cfg(test)]
pub(crate) mod mock {
    use super::{Transport, TransportBuilder, TransportReceiver, TransportSender, sealed};
    use crate::client::config::ServerConfig;
    use crate::error::{Error, Result};
    use bytes::Bytes;
    use tokio::sync::mpsc;

    /// Test-side handle to one mock ADS stream.
    pub(crate) struct MockServer {
        /// Requests the worker sent (initial requests, ACKs, subscription changes).
        pub(crate) requests: mpsc::UnboundedReceiver<Bytes>,
        /// Responses to push to the worker.
        pub(crate) responses: mpsc::UnboundedSender<Result<Option<Bytes>>>,
    }

    /// Returns a transport builder for the worker plus the receiver on which
    /// the test obtains a [`MockServer`] for every stream the worker opens.
    pub(crate) fn mock_transport() -> (MockTransportBuilder, mpsc::UnboundedReceiver<MockServer>) {
        let (servers_tx, servers_rx) = mpsc::unbounded_channel();
        (
            MockTransportBuilder {
                servers: servers_tx,
            },
            servers_rx,
        )
    }

    pub(crate) struct MockTransportBuilder {
        servers: mpsc::UnboundedSender<MockServer>,
    }

    impl TransportBuilder for MockTransportBuilder {
        type Transport = MockTransport;

        async fn build(&self, _server: &ServerConfig) -> Result<Self::Transport> {
            Ok(MockTransport {
                servers: self.servers.clone(),
            })
        }
    }

    pub(crate) struct MockTransport {
        servers: mpsc::UnboundedSender<MockServer>,
    }

    impl Transport for MockTransport {
        type Sender = MockSender;
        type Receiver = MockReceiver;

        async fn new_stream(&self) -> Result<(Self::Sender, Self::Receiver)> {
            let (req_tx, req_rx) = mpsc::unbounded_channel();
            let (resp_tx, resp_rx) = mpsc::unbounded_channel();
            let _ = self.servers.send(MockServer {
                requests: req_rx,
                responses: resp_tx,
            });
            Ok((
                MockSender { requests: req_tx },
                MockReceiver { responses: resp_rx },
            ))
        }
    }

    pub(crate) struct MockSender {
        requests: mpsc::UnboundedSender<Bytes>,
    }

    impl sealed::Sealed for MockSender {}

    impl TransportSender for MockSender {
        async fn send(&mut self, request: Bytes) -> Result<()> {
            self.requests
                .send(request)
                .map_err(|_| Error::Connection("mock stream closed".into()))
        }
    }

    pub(crate) struct MockReceiver {
        responses: mpsc::UnboundedReceiver<Result<Option<Bytes>>>,
    }

    impl sealed::Sealed for MockReceiver {}

    impl TransportReceiver for MockReceiver {
        async fn recv(&mut self) -> Result<Option<Bytes>> {
            match self.responses.recv().await {
                Some(result) => result,
                None => Ok(None),
            }
        }
    }
}
