//! `tonic` based transport implementation.
//!
//! This transport uses tonic's low-level `Grpc` client with a `BytesCodec`
//! to send and receive raw bytes, allowing the xDS client layer to handle
//! serialization/deserialization independently.

use crate::client::config::ServerConfig;
use crate::error::{Error, Result};
use crate::transport::{Transport, TransportBuilder, TransportStream};
use bytes::{Buf, BufMut, Bytes};
use http::uri::PathAndQuery;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tonic::client::Grpc;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::Channel;
use tonic::{Status, Streaming};

/// The gRPC path for the ADS StreamAggregatedResources RPC.
const ADS_PATH: &str =
    "/envoy.service.discovery.v3.AggregatedDiscoveryService/StreamAggregatedResources";

const ADS_CHANNEL_BUFFER_SIZE: usize = 16;

/// A codec that passes bytes through without serialization.
///
/// This allows us to handle serialization in the xDS client layer
/// rather than in the transport layer.
#[derive(Debug, Clone, Copy)]
struct BytesCodec;

impl Codec for BytesCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = BytesEncoder;
    type Decoder = BytesDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        BytesEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        BytesDecoder
    }
}

#[derive(Debug)]
struct BytesEncoder;

impl Encoder for BytesEncoder {
    type Item = Bytes;
    type Error = Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut EncodeBuf<'_>,
    ) -> std::result::Result<(), Self::Error> {
        dst.put_slice(&item);
        Ok(())
    }
}

#[derive(Debug)]
struct BytesDecoder;

impl Decoder for BytesDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(
        &mut self,
        src: &mut DecodeBuf<'_>,
    ) -> std::result::Result<Option<Self::Item>, Self::Error> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// Factory for creating ADS streams using tonic.
#[derive(Clone, Debug)]
pub struct TonicTransport {
    channel: Channel,
}

impl TonicTransport {
    /// Create a transport from an existing tonic [`Channel`].
    ///
    /// Use this when you need custom channel configuration (e.g., TLS, timeouts).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use tonic::transport::{Certificate, Channel, ClientTlsConfig};
    ///
    /// let tls = ClientTlsConfig::new()
    ///     .ca_certificate(Certificate::from_pem(ca_cert))
    ///     .domain_name("xds.example.com");
    ///
    /// let channel = Channel::from_static("https://xds.example.com:443")
    ///     .tls_config(tls)?
    ///     .connect()
    ///     .await?;
    ///
    /// let transport = TonicTransport::from_channel(channel);
    /// ```
    pub fn from_channel(channel: Channel) -> Self {
        Self { channel }
    }

    /// Connect to an xDS server with default settings.
    ///
    /// For custom configuration (TLS, timeouts, etc.), use [`from_channel`](Self::from_channel).
    pub async fn connect(uri: impl Into<String>) -> Result<Self> {
        let uri: String = uri.into();
        let channel = Channel::from_shared(uri)
            .map_err(|e| Error::Connection(e.to_string()))?
            .connect()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { channel })
    }
}

/// Builder for creating [`TonicTransport`] instances.
///
/// This implements [`TransportBuilder`] and can be used with [`XdsClientBuilder`]
/// to enable server fallback support.
///
/// For connections requiring TLS or custom channel configuration, see the
/// example in [`TonicTransport::from_channel`].
///
/// # Example
///
/// ```ignore
/// use xds_client::{ClientConfig, Node, TonicTransportBuilder, XdsClient};
///
/// let transport_builder = TonicTransportBuilder::new();
/// let config = ClientConfig::new(node, "http://xds.example.com:18000");
/// let client = XdsClient::builder(config, transport_builder, codec, runtime).build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct TonicTransportBuilder {
    // Future extensions:
    // - TLS configuration (requires tonic TLS feature)
    // - Connection timeout settings
    // - Keep-alive configuration
    // - Connection pooling settings
    // - Per-server credential overrides (via ServerConfig.extensions)
}

impl TonicTransportBuilder {
    /// Create a new transport builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TransportBuilder for TonicTransportBuilder {
    type Transport = TonicTransport;

    async fn build(&self, server: &ServerConfig) -> Result<Self::Transport> {
        let channel = Channel::from_shared(server.uri().to_string())
            .map_err(|e| Error::Connection(e.to_string()))?
            .connect()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(TonicTransport::from_channel(channel))
    }
}

impl Transport for TonicTransport {
    type Stream = TonicAdsStream;

    async fn new_stream(&self, initial_requests: Vec<Bytes>) -> Result<Self::Stream> {
        let mut grpc = Grpc::new(self.channel.clone());

        grpc.ready()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        let (tx, rx) = mpsc::channel::<Bytes>(ADS_CHANNEL_BUFFER_SIZE);

        // Create a stream that first yields initial requests, then reads from the channel.
        // This ensures data is available immediately when the stream is polled,
        // preventing deadlock with servers that don't send response headers
        // until they receive the first request message.
        let initial_stream = tokio_stream::iter(initial_requests);
        let channel_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let request_stream = initial_stream.chain(channel_stream);

        let path = PathAndQuery::from_static(ADS_PATH);

        let response = grpc
            .streaming(tonic::Request::new(request_stream), path, BytesCodec)
            .await
            .map_err(Error::Stream)?;

        Ok(TonicAdsStream {
            sender: tx,
            receiver: response.into_inner(),
        })
    }
}

/// A bidirectional ADS stream backed by tonic.
#[derive(Debug)]
pub struct TonicAdsStream {
    sender: mpsc::Sender<Bytes>,
    receiver: Streaming<Bytes>,
}

impl TransportStream for TonicAdsStream {
    async fn send(&mut self, request: Bytes) -> Result<()> {
        self.sender
            .send(request)
            .await
            .map_err(|_| Error::StreamClosed)?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Bytes>> {
        match self.receiver.message().await {
            Ok(msg) => Ok(msg),
            Err(status) => Err(Error::Stream(status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::service::discovery::v3::{
        aggregated_discovery_service_server::{
            AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
        },
        DeltaDiscoveryRequest, DeltaDiscoveryResponse, DiscoveryRequest, DiscoveryResponse,
    };
    use prost::Message;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tokio_stream::Stream;
    use tonic::{Request, Response, Status};

    /// Mock ADS server that echoes back a response for each request.
    struct MockAdsServer;

    #[tonic::async_trait]
    impl AggregatedDiscoveryService for MockAdsServer {
        type StreamAggregatedResourcesStream =
            Pin<Box<dyn Stream<Item = std::result::Result<DiscoveryResponse, Status>> + Send>>;

        async fn stream_aggregated_resources(
            &self,
            request: Request<tonic::Streaming<DiscoveryRequest>>,
        ) -> std::result::Result<Response<Self::StreamAggregatedResourcesStream>, Status> {
            let mut inbound = request.into_inner();

            let outbound = async_stream::try_stream! {
                while let Some(req) = inbound.next().await {
                    let req = req?;
                    let response = DiscoveryResponse {
                        version_info: "1".to_string(),
                        type_url: req.type_url.clone(),
                        nonce: "nonce-1".to_string(),
                        resources: vec![],
                        ..Default::default()
                    };
                    yield response;
                }
            };

            Ok(Response::new(Box::pin(outbound)))
        }

        type DeltaAggregatedResourcesStream =
            Pin<Box<dyn Stream<Item = std::result::Result<DeltaDiscoveryResponse, Status>> + Send>>;

        async fn delta_aggregated_resources(
            &self,
            _request: Request<tonic::Streaming<DeltaDiscoveryRequest>>,
        ) -> std::result::Result<Response<Self::DeltaAggregatedResourcesStream>, Status> {
            Err(Status::unimplemented("delta not supported in mock"))
        }
    }

    async fn start_mock_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(AggregatedDiscoveryServiceServer::new(MockAdsServer))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        addr
    }

    #[tokio::test]
    async fn test_tonic_transport_connect_and_stream() {
        let addr = start_mock_server().await;
        let uri = format!("http://{addr}");

        let transport = TonicTransport::connect(&uri).await.unwrap();

        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            resource_names: vec!["listener-1".to_string()],
            ..Default::default()
        };
        let request_bytes: Bytes = request.encode_to_vec().into();

        let mut stream = transport.new_stream(vec![request_bytes]).await.unwrap();

        let response_bytes = stream.recv().await.unwrap().unwrap();
        let response = DiscoveryResponse::decode(response_bytes).unwrap();

        assert_eq!(response.version_info, "1");
        assert_eq!(
            response.type_url,
            "type.googleapis.com/envoy.config.listener.v3.Listener"
        );
        assert_eq!(response.nonce, "nonce-1");
    }
}
