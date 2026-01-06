//! ADS worker that manages the xDS stream.
//!
//! This is a skeleton implementation showing how the transport and codec
//! integrate. Full subscription management will be added in future PRs.

use crate::codec::XdsCodec;
use crate::error::Result;
use crate::message::{DiscoveryRequest, DiscoveryResponse, Node};
use crate::transport::TransportStream;

/// The ADS worker manages a single xDS stream.
///
/// It handles:
/// - Sending discovery requests (subscriptions)
/// - Receiving discovery responses
/// - Version/nonce tracking for ACK/NACK
#[derive(Debug)]
pub struct AdsWorker<S, C> {
    stream: S,
    codec: C,
    node: Option<Node>,
    // TODO: Add subscription tracking
    // subscriptions: HashMap<String, HashSet<String>>,  // type_url -> resource_names
    // TODO: Add version/nonce tracking
    // versions: HashMap<String, String>,  // type_url -> version_info
    // nonces: HashMap<String, String>,    // type_url -> response_nonce
}

impl<S: TransportStream, C: XdsCodec> AdsWorker<S, C> {
    /// Create a new worker with the given stream and codec.
    pub fn new(stream: S, codec: C, node: Option<Node>) -> Self {
        Self { stream, codec, node }
    }

    /// Send a discovery request.
    ///
    /// The codec serializes the request to bytes, then the transport sends it.
    pub async fn send_request(&mut self, request: DiscoveryRequest) -> Result<()> {
        let bytes = self.codec.encode_request(&request)?;
        self.stream.send(bytes).await
    }

    /// Receive a discovery response.
    ///
    /// The transport receives bytes, then the codec deserializes them.
    pub async fn recv_response(&mut self) -> Result<Option<DiscoveryResponse>> {
        let bytes = match self.stream.recv().await? {
            Some(b) => b,
            None => return Ok(None),
        };
        let response = self.codec.decode_response(bytes)?;
        Ok(Some(response))
    }

    /// Subscribe to resources of a given type.
    pub async fn subscribe(&mut self, type_url: &str, resource_names: Vec<String>) -> Result<()> {
        // TODO: Track subscription in self.subscriptions
        // TODO: Get current version/nonce from tracking state
        let (version_info, response_nonce) = self.get_version_nonce(type_url);

        let request = DiscoveryRequest {
            node: self.node.clone(),
            type_url: type_url.to_string(),
            resource_names,
            version_info,
            response_nonce,
            error_detail: None,
        };

        self.send_request(request).await
    }

    /// Send ACK for a received response.
    pub async fn ack(&mut self, response: &DiscoveryResponse) -> Result<()> {
        // TODO: Update version/nonce tracking state
        // TODO: Get currently subscribed resource names for this type_url
        let resource_names = self.get_subscribed_names(&response.type_url);

        let request = DiscoveryRequest {
            node: None,
            type_url: response.type_url.clone(),
            resource_names,
            version_info: response.version_info.clone(),
            response_nonce: response.nonce.clone(),
            error_detail: None,
        };

        self.send_request(request).await
    }

    /// Get the current version and nonce for a type_url.
    fn get_version_nonce(&self, _type_url: &str) -> (String, String) {
        // TODO: Look up from self.versions and self.nonces
        todo!("implement version/nonce tracking")
    }

    /// Get the currently subscribed resource names for a type_url.
    fn get_subscribed_names(&self, _type_url: &str) -> Vec<String> {
        // TODO: Look up from self.subscriptions
        todo!("implement subscription tracking")
    }

    /// Run the worker event loop.
    pub async fn run(&mut self) -> Result<()> {
        // TODO: Implement the main event loop that:
        // - Receives responses from the stream
        // - Dispatches resources to watchers
        // - Sends ACK/NACK
        // - Handles reconnection
        todo!("implement worker event loop")
    }
}

