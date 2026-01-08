//! Crate-owned xDS message types.
//!
//! These types are codegen-agnostic and serve as the interface between
//! the xDS client logic and the codec layer. The codec converts these
//! to/from the wire format (e.g., prost/envoy-types or google-protobuf).

use bytes::Bytes;

/// A discovery request to send to the xDS server.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryRequest {
    /// The version_info provided in the most recent successfully processed
    /// response for this type, or empty for the first request.
    pub version_info: String,
    /// The node making the request.
    pub node: Option<Node>,
    /// List of resource names to subscribe to.
    pub resource_names: Vec<String>,
    /// Type URL of the resource being requested.
    pub type_url: String,
    /// The nonce from the most recent successfully processed response,
    /// or empty for the first request.
    pub response_nonce: String,
    /// Error details if this is a NACK (negative acknowledgment).
    pub error_detail: Option<ErrorDetail>,
}

/// A discovery response from the xDS server.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryResponse {
    /// The version of the response data.
    pub version_info: String,
    /// The response resources wrapped as Any protos.
    pub resources: Vec<ResourceAny>,
    /// Type URL of the resources.
    pub type_url: String,
    /// Nonce for this response, to be echoed back in the next request.
    pub nonce: String,
}

/// A resource wrapped as google.protobuf.Any.
#[derive(Debug, Clone)]
pub struct ResourceAny {
    /// Type URL of the resource.
    pub type_url: String,
    /// Serialized resource bytes.
    pub value: Bytes,
}

/// Node identification for the client.
#[derive(Debug, Clone, Default)]
pub struct Node {
    /// An opaque node identifier.
    pub id: String,
    /// The cluster the node belongs to.
    pub cluster: String,
    /// Locality specifying where the node is running.
    pub locality: Option<Locality>,
}

/// Locality information identifying where a node is running.
#[derive(Debug, Clone, Default)]
pub struct Locality {
    /// Region the node is in.
    pub region: String,
    /// Zone within the region.
    pub zone: String,
    /// Sub-zone within the zone.
    pub sub_zone: String,
}

/// Error details for NACK responses.
#[derive(Debug, Clone)]
pub struct ErrorDetail {
    /// gRPC status code.
    pub code: i32,
    /// Error message.
    pub message: String,
}
