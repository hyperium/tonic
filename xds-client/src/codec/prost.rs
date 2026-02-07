//! Prost-based codec using envoy-types.

use crate::codec::XdsCodec;
use crate::error::{Error, Result};
use crate::message::{DiscoveryRequest, DiscoveryResponse, ResourceAny};
use bytes::Bytes;
use prost::Message;

/// A codec that uses prost/envoy-types for serialization.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProstCodec;

impl XdsCodec for ProstCodec {
    fn encode_request(&self, request: &DiscoveryRequest) -> Result<Bytes> {
        use envoy_types::pb::envoy::config::core::v3 as core;
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;
        use envoy_types::pb::google::rpc::Status;

        let proto_request = discovery::DiscoveryRequest {
            version_info: request.version_info.clone(),
            node: request.node.as_ref().map(|n| core::Node {
                id: n.id.clone(),
                cluster: n.cluster.clone(),
                locality: n.locality.as_ref().map(|l| core::Locality {
                    region: l.region.clone(),
                    zone: l.zone.clone(),
                    sub_zone: l.sub_zone.clone(),
                }),
                ..Default::default()
            }),
            resource_names: request.resource_names.clone(),
            type_url: request.type_url.clone(),
            response_nonce: request.response_nonce.clone(),
            error_detail: request.error_detail.as_ref().map(|e| Status {
                code: e.code,
                message: e.message.clone(),
                details: vec![],
            }),
            ..Default::default()
        };

        Ok(proto_request.encode_to_vec().into())
    }

    fn decode_response(&self, bytes: Bytes) -> Result<DiscoveryResponse> {
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;

        let proto_response = discovery::DiscoveryResponse::decode(bytes).map_err(Error::Decode)?;

        Ok(DiscoveryResponse {
            version_info: proto_response.version_info,
            resources: proto_response
                .resources
                .into_iter()
                .map(|any| ResourceAny {
                    type_url: any.type_url,
                    value: any.value.into(),
                })
                .collect(),
            type_url: proto_response.type_url,
            nonce: proto_response.nonce,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ErrorDetail, Locality, Node};

    #[test]
    fn test_encode_request_minimal() {
        let codec = ProstCodec;
        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            resource_names: vec!["listener-1".to_string()],
            ..Default::default()
        };

        let bytes = codec.encode_request(&request).unwrap();
        assert!(!bytes.is_empty());

        // Verify we can decode it back with prost
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;
        let decoded = discovery::DiscoveryRequest::decode(bytes).unwrap();
        assert_eq!(decoded.type_url, request.type_url);
        assert_eq!(decoded.resource_names, request.resource_names);
    }

    #[test]
    fn test_encode_request_with_node() {
        let codec = ProstCodec;
        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.cluster.v3.Cluster".to_string(),
            node: Some(Node {
                id: "node-1".to_string(),
                cluster: "cluster-1".to_string(),
                locality: Some(Locality {
                    region: "us-west".to_string(),
                    zone: "us-west-1a".to_string(),
                    sub_zone: "rack-1".to_string(),
                }),
            }),
            ..Default::default()
        };

        let bytes = codec.encode_request(&request).unwrap();

        use envoy_types::pb::envoy::service::discovery::v3 as discovery;
        let decoded = discovery::DiscoveryRequest::decode(bytes).unwrap();
        let node = decoded.node.unwrap();
        assert_eq!(node.id, "node-1");
        assert_eq!(node.cluster, "cluster-1");
        let locality = node.locality.unwrap();
        assert_eq!(locality.region, "us-west");
        assert_eq!(locality.zone, "us-west-1a");
        assert_eq!(locality.sub_zone, "rack-1");
    }

    #[test]
    fn test_decode_response() {
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;
        use envoy_types::pb::google::protobuf::Any;

        let proto_response = discovery::DiscoveryResponse {
            version_info: "1".to_string(),
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            nonce: "nonce-1".to_string(),
            resources: vec![Any {
                type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
                value: b"fake-listener-bytes".to_vec(),
            }],
            ..Default::default()
        };

        let bytes: Bytes = proto_response.encode_to_vec().into();

        let codec = ProstCodec;
        let response = codec.decode_response(bytes).unwrap();

        assert_eq!(response.version_info, "1");
        assert_eq!(
            response.type_url,
            "type.googleapis.com/envoy.config.listener.v3.Listener"
        );
        assert_eq!(response.nonce, "nonce-1");
        assert_eq!(response.resources.len(), 1);
        assert_eq!(
            response.resources[0].type_url,
            "type.googleapis.com/envoy.config.listener.v3.Listener"
        );
        assert_eq!(response.resources[0].value.as_ref(), b"fake-listener-bytes");
    }

    #[test]
    fn test_roundtrip() {
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;

        let codec = ProstCodec;

        let request = DiscoveryRequest {
            version_info: "42".to_string(),
            type_url: "type.googleapis.com/test.Resource".to_string(),
            resource_names: vec!["res-1".to_string(), "res-2".to_string()],
            response_nonce: "nonce-abc".to_string(),
            error_detail: Some(ErrorDetail {
                code: 3, // INVALID_ARGUMENT
                message: "validation failed".to_string(),
            }),
            ..Default::default()
        };

        let request_bytes = codec.encode_request(&request).unwrap();

        let proto_request = discovery::DiscoveryRequest::decode(request_bytes).unwrap();
        assert_eq!(proto_request.version_info, "42");
        assert_eq!(proto_request.response_nonce, "nonce-abc");
        let error = proto_request.error_detail.unwrap();
        assert_eq!(error.code, 3);
        assert_eq!(error.message, "validation failed");
    }
}
