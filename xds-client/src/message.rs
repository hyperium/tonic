//! Crate-owned xDS message types.
//!
//! These types are codegen-agnostic and serve as the interface between
//! the xDS client logic and the codec layer. The codec converts these
//! to/from the wire format (e.g., prost/envoy-types or google-protobuf).

use bytes::Bytes;

/// A discovery request to send to the xDS server.
#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    /// The version_info provided in the most recent successfully processed
    /// response for this type, or empty for the first request.
    pub version_info: String,
    /// The node making the request.
    pub node: Node,
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
#[derive(Debug, Clone)]
pub struct Node {
    /// An opaque node identifier.
    pub id: Option<String>,
    /// The cluster the node belongs to.
    pub cluster: Option<String>,
    /// Locality specifying where the node is running.
    pub locality: Option<Locality>,
    /// Free-form string identifying the client type (e.g., "envoy", "grpc").
    pub user_agent_name: String,
    /// Version of the client.
    pub user_agent_version: String,
}

impl Node {
    /// Create a new Node with the required user agent fields.
    ///
    /// Other fields (id, cluster, locality) can be set using builder methods.
    pub fn new(user_agent_name: impl Into<String>, user_agent_version: impl Into<String>) -> Self {
        Self {
            id: None,
            cluster: None,
            locality: None,
            user_agent_name: user_agent_name.into(),
            user_agent_version: user_agent_version.into(),
        }
    }

    /// Set the node ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Set the cluster.
    pub fn with_cluster(mut self, cluster: impl Into<String>) -> Self {
        self.cluster = Some(cluster.into());
        self
    }

    /// Set the locality.
    pub fn with_locality(mut self, locality: Locality) -> Self {
        self.locality = Some(locality);
        self
    }
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
