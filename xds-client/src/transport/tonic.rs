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

//! `tonic` based transport implementation.
//!
//! This transport uses tonic's low-level `Grpc` client with a `BytesCodec`
//! to send and receive raw bytes, allowing the xDS client layer to handle
//! serialization/deserialization independently.

use crate::client::config::ServerConfig;
use crate::error::{Error, Result};
use crate::transport::{Transport, TransportBuilder, TransportReceiver, TransportSender};
use bytes::{Buf, BufMut, Bytes};
use http::uri::PathAndQuery;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::client::Grpc;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::{Channel, Endpoint};
use tonic::{Status, Streaming};

/// Per-stream call credentials for the ADS stream (e.g. a bearer token).
///
/// Attached on each (re)connect, only when the channel is secure.
#[tonic::async_trait]
pub trait TonicCallCredentials: Send + Sync + std::fmt::Debug + 'static {
    /// Generates the authentication metadata for a specific call.
    async fn get_request_metadata(
        &self,
        metadata: &mut tonic::metadata::MetadataMap,
    ) -> std::result::Result<(), Status>;

    /// Whether these credentials require a secure (TLS) transport.
    fn requires_secure_transport(&self) -> bool {
        // Note: a bool simplification of the `grpc` crate's
        // `CallCredentials::minimum_channel_security_level` (`SecurityLevel`).
        true
    }
}

/// The gRPC path for the ADS StreamAggregatedResources RPC.
const ADS_PATH: &str =
    "/envoy.service.discovery.v3.AggregatedDiscoveryService/StreamAggregatedResources";

const ADS_CHANNEL_BUFFER_SIZE: usize = 16;

/// Default timeout for establishing the TCP/TLS connection to the xDS server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default HTTP/2 keepalive PING interval on the ADS channel.
///
/// The ADS stream is mostly idle from the client's perspective (the server
/// only pushes on resource changes), so without keepalives a half-open
/// connection — e.g. after an xDS server restart where the RST/GOAWAY was
/// lost, or a dropped conntrack/NAT entry — is undetectable: `recv()` on the
/// stream pends forever and the client keeps serving its last known
/// resources. Keepalives surface such connections as transport errors, which
/// triggers the worker's reconnect + re-subscribe path.
const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Default time to wait for a keepalive PING ack before closing the connection.
const DEFAULT_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);

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
    call_creds: Option<Arc<dyn TonicCallCredentials>>,
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
}

impl TonicTransport {
    /// Create a transport from an existing tonic [`Channel`].
    ///
    /// Use this when you need custom channel configuration (e.g., TLS, timeouts).
    /// Call credentials are not supported here, since the channel's security
    /// cannot be verified; use [`TonicTransportBuilder`] for them.
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
        Self {
            channel,
            call_creds: None,
            max_decoding_message_size: None,
            max_encoding_message_size: None,
        }
    }

    /// Connect to an xDS server with default settings.
    ///
    /// For custom configuration (TLS, call credentials), use [`TonicTransportBuilder`];
    /// for a pre-built channel, use [`from_channel`](Self::from_channel).
    pub async fn connect(uri: impl Into<String>) -> Result<Self> {
        let server = ServerConfig::new(uri.into());
        TonicTransportBuilder::new().build(&server).await
    }
}

/// Builder for creating [`TonicTransport`] instances.
///
/// This implements [`TransportBuilder`] and can be used with
/// [`XdsClientBuilder`](crate::XdsClientBuilder) to enable server fallback support.
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
///
/// # TLS
///
/// Enable the `tonic-tls-ring` or `tonic-tls-aws-lc` feature and call
/// `with_tls_config`:
///
/// ```ignore
/// use tonic::transport::ClientTlsConfig;
/// use xds_client::TonicTransportBuilder;
///
/// let builder = TonicTransportBuilder::new()
///     .with_tls_config(ClientTlsConfig::new().with_enabled_roots());
/// ```
#[derive(Clone)]
pub struct TonicTransportBuilder {
    // Future extensions:
    // - Connection pooling settings
    // - Per-server credential overrides (via ServerConfig.extensions)
    #[cfg(feature = "_tonic-tls-any")]
    tls_config_factory: Option<TlsConfigFactory>,

    /// Per-stream call credentials for the ADS stream.
    call_creds: Option<Arc<dyn TonicCallCredentials>>,

    /// Timeout for establishing the connection to the xDS server.
    connect_timeout: Duration,

    /// HTTP/2 keepalive PING interval; `None` disables keepalives.
    keep_alive_interval: Option<Duration>,

    /// Time to wait for a keepalive PING ack before closing the connection.
    keep_alive_timeout: Duration,

    /// Maximum size of a decoded ADS response; `None` leaves tonic's default.
    max_decoding_message_size: Option<usize>,

    /// Maximum size of an encoded ADS request; `None` leaves tonic's default.
    max_encoding_message_size: Option<usize>,
}

#[cfg(feature = "_tonic-tls-any")]
type TlsConfigFactory =
    Arc<dyn Fn(&ServerConfig) -> Result<tonic::transport::ClientTlsConfig> + Send + Sync + 'static>;

impl std::fmt::Debug for TonicTransportBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut builder = f.debug_struct("TonicTransportBuilder");
        #[cfg(feature = "_tonic-tls-any")]
        builder.field("tls_configured", &self.tls_config_factory.is_some());
        builder
            .field("call_creds", &self.call_creds)
            .field("connect_timeout", &self.connect_timeout)
            .field("keep_alive_interval", &self.keep_alive_interval)
            .field("keep_alive_timeout", &self.keep_alive_timeout)
            .finish()
    }
}

impl Default for TonicTransportBuilder {
    fn default() -> Self {
        Self {
            #[cfg(feature = "_tonic-tls-any")]
            tls_config_factory: None,
            call_creds: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            keep_alive_interval: Some(DEFAULT_KEEP_ALIVE_INTERVAL),
            keep_alive_timeout: DEFAULT_KEEP_ALIVE_TIMEOUT,
            max_decoding_message_size: None,
            max_encoding_message_size: None,
        }
    }
}

impl TonicTransportBuilder {
    /// Create a new transport builder with default (plaintext) settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the timeout for establishing the connection to the xDS server.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Configure HTTP/2 keepalives on the ADS channel.
    ///
    /// A PING is sent every `interval` (even while the stream is idle); if no
    /// ack arrives within `timeout` the connection is closed, surfacing a
    /// transport error that triggers reconnect + re-subscription. Pass
    /// `interval = None` to disable keepalives.
    pub fn with_keep_alive(mut self, interval: Option<Duration>, timeout: Duration) -> Self {
        self.keep_alive_interval = interval;
        self.keep_alive_timeout = timeout;
        self
    }

    /// Set the TLS configuration for connections to the xDS server.
    ///
    /// When set, all connections created by this builder will use TLS
    /// with the provided configuration.
    #[cfg(feature = "_tonic-tls-any")]
    pub fn with_tls_config(mut self, tls_config: tonic::transport::ClientTlsConfig) -> Self {
        self.tls_config_factory = Some(Arc::new(move |_| Ok(tls_config.clone())));
        self
    }

    /// Construct the TLS configuration each time a connection is created.
    ///
    /// The factory is called from [`build`](TransportBuilder::build), including
    /// every reconnect, so callers can supply refreshed certificate material.
    #[cfg(feature = "_tonic-tls-any")]
    pub fn with_tls_config_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(&ServerConfig) -> Result<tonic::transport::ClientTlsConfig> + Send + Sync + 'static,
    {
        self.tls_config_factory = Some(Arc::new(factory));
        self
    }

    /// Set per-stream call credentials for the ADS stream (e.g. `google_default`).
    ///
    /// Attached on each (re)connect, only over a secure channel; over an insecure
    /// channel, [`build`](TransportBuilder::build) fails. Not refreshed mid-stream.
    pub fn with_call_credentials(mut self, creds: Arc<dyn TonicCallCredentials>) -> Self {
        self.call_creds = Some(creds);
        self
    }

    /// Limit the size of a decoded ADS response, in bytes.
    ///
    /// Defaults to tonic's 4 MiB. A control plane whose response exceeds the
    /// limit breaks the stream, so raise it to match the largest resource set
    /// the server sends.
    pub fn with_max_decoding_message_size(mut self, limit: usize) -> Self {
        self.max_decoding_message_size = Some(limit);
        self
    }

    /// Limit the size of an encoded ADS request, in bytes.
    ///
    /// Defaults to tonic's `usize::MAX`.
    pub fn with_max_encoding_message_size(mut self, limit: usize) -> Self {
        self.max_encoding_message_size = Some(limit);
        self
    }

    /// Prepend `https://` to a scheme-less `server_uri` on the secure path.
    ///
    /// Bootstrap URIs like `trafficdirector.googleapis.com:443` parse with no scheme,
    /// so `Endpoint` won't negotiate TLS. A scheme lets it, and tonic derive SNI from
    /// `uri.host()`. Non-`http::Uri` inputs (`unix://`) and plaintext are left as-is.
    fn ensure_secure_server_uri(raw: &str, secure: bool) -> String {
        if secure
            && let Ok(uri) = raw.parse::<http::Uri>()
            && uri.scheme().is_none()
        {
            return format!("https://{raw}");
        }
        raw.to_string()
    }
}

impl TransportBuilder for TonicTransportBuilder {
    type Transport = TonicTransport;

    async fn build(&self, server: &ServerConfig) -> Result<Self::Transport> {
        // With no TLS backend compiled in, `tls_configured` stays false.
        #[cfg(feature = "_tonic-tls-any")]
        let tls_configured = self.tls_config_factory.is_some();
        #[cfg(not(feature = "_tonic-tls-any"))]
        let tls_configured = false;

        let uri = Self::ensure_secure_server_uri(server.uri(), tls_configured);

        // tonic handshakes for an `https` URI alone, so the scheme decides
        // whether the channel is encrypted.
        let secure = uri
            .parse::<http::Uri>()
            .is_ok_and(|uri| uri.scheme_str() == Some("https"));

        // Require the scheme and the TLS config to agree, so the caller gets
        // the channel they asked for. `ensure_secure_server_uri` has already
        // upgraded the scheme-less form, leaving only real conflicts here.
        if tls_configured && !secure {
            return Err(Error::Connection(format!(
                "TLS is configured but server URI '{uri}' connects in plaintext; \
                 use an `https://` or scheme-less URI"
            )));
        }
        if secure && !tls_configured {
            return Err(Error::Connection(format!(
                "server URI '{uri}' requires TLS but no TLS config is set"
            )));
        }

        // Fail before connecting, so credentials stay off an insecure channel.
        if let Some(creds) = &self.call_creds
            && creds.requires_secure_transport()
            && !secure
        {
            return Err(Error::CallCredentials(
                "call credentials require a secure transport".into(),
            ));
        }

        // `Endpoint::from_shared` routes `unix://` URIs to tonic's UDS connector.
        // Required for control planes like Istio's grpc-agent that ship `unix:///etc/istio/proxy/XDS`.
        let endpoint = Endpoint::from_shared(uri).map_err(|e| Error::Connection(e.to_string()))?;

        let mut endpoint = endpoint.connect_timeout(self.connect_timeout);
        if let Some(interval) = self.keep_alive_interval {
            endpoint = endpoint
                .http2_keep_alive_interval(interval)
                .keep_alive_timeout(self.keep_alive_timeout)
                .keep_alive_while_idle(true);
        }

        #[cfg(feature = "_tonic-tls-any")]
        let endpoint = match &self.tls_config_factory {
            Some(factory) => endpoint
                .tls_config(factory(server)?)
                .map_err(|error| Error::TlsConfig(Box::new(error)))?,
            None => endpoint,
        };

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(TonicTransport {
            channel,
            call_creds: self.call_creds.clone(),
            max_decoding_message_size: self.max_decoding_message_size,
            max_encoding_message_size: self.max_encoding_message_size,
        })
    }
}

type StreamingFuture =
    Box<dyn Future<Output = std::result::Result<Streaming<Bytes>, Status>> + Send>;

impl Transport for TonicTransport {
    type Sender = TonicAdsSender;
    type Receiver = TonicAdsReceiver;

    async fn new_stream(&self) -> Result<(Self::Sender, Self::Receiver)> {
        let mut grpc = Grpc::new(self.channel.clone());

        if let Some(limit) = self.max_decoding_message_size {
            grpc = grpc.max_decoding_message_size(limit);
        }
        if let Some(limit) = self.max_encoding_message_size {
            grpc = grpc.max_encoding_message_size(limit);
        }

        grpc.ready()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        let (tx, rx) = mpsc::channel::<Bytes>(ADS_CHANNEL_BUFFER_SIZE);
        let channel_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let path = PathAndQuery::from_static(ADS_PATH);
        let mut request = tonic::Request::new(channel_stream);

        // Inject the configured call credentials.
        if let Some(creds) = &self.call_creds {
            creds
                .get_request_metadata(request.metadata_mut())
                .await
                .map_err(|e| Error::CallCredentials(e.to_string()))?;
        }

        let connect_fut: StreamingFuture = Box::new(async move {
            let response = grpc.streaming(request, path, BytesCodec).await?;
            Ok(response.into_inner())
        });

        let receiver = TonicAdsReceiver {
            state: TonicReceiverState::Connecting(Some(connect_fut)),
        };

        Ok((TonicAdsSender { sender: tx }, receiver))
    }
}

/// Sending half of a `TonicTransport` ADS stream.
#[derive(Debug)]
pub struct TonicAdsSender {
    sender: mpsc::Sender<Bytes>,
}

impl TransportSender for TonicAdsSender {
    async fn send(&mut self, request: Bytes) -> Result<()> {
        self.sender
            .send(request)
            .await
            .map_err(|_| Error::StreamClosed)?;
        Ok(())
    }
}

enum TonicReceiverState {
    Connecting(Option<StreamingFuture>),
    Connected(Streaming<Bytes>),
}

impl std::fmt::Debug for TonicReceiverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting(_) => f.write_str("Connecting(...)"),
            Self::Connected(s) => f.debug_tuple("Connected").field(s).finish(),
        }
    }
}

/// Receiving half of a `TonicTransport` ADS stream.
#[derive(Debug)]
pub struct TonicAdsReceiver {
    state: TonicReceiverState,
}

impl TransportReceiver for TonicAdsReceiver {
    async fn recv(&mut self) -> Result<Option<Bytes>> {
        loop {
            match &mut self.state {
                TonicReceiverState::Connecting(opt) => {
                    let fut = opt.take().ok_or(Error::StreamClosed)?;
                    match Pin::from(fut).await {
                        Ok(streaming) => {
                            self.state = TonicReceiverState::Connected(streaming);
                        }
                        Err(status) => {
                            return Err(Error::Stream(status));
                        }
                    }
                }
                TonicReceiverState::Connected(streaming) => {
                    return streaming.message().await.map_err(Error::Stream);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::envoy::service::discovery::v3::{
        DeltaDiscoveryRequest, DeltaDiscoveryResponse, DiscoveryRequest, DiscoveryResponse,
        aggregated_discovery_service_server::{
            AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
        },
    };
    use prost::Message;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::Arc;
    #[cfg(feature = "_tonic-tls-any")]
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use tokio_stream::Stream;
    use tokio_stream::StreamExt as _;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, Response, Status};

    /// Mock ADS server that echoes back a response for each request.
    #[derive(Default)]
    struct MockAdsServer {
        expected_auth: Option<String>,
        /// Bytes of filler to attach to each response, to exercise size limits.
        response_padding: usize,
    }

    #[tonic::async_trait]
    impl AggregatedDiscoveryService for MockAdsServer {
        type StreamAggregatedResourcesStream =
            Pin<Box<dyn Stream<Item = std::result::Result<DiscoveryResponse, Status>> + Send>>;

        async fn stream_aggregated_resources(
            &self,
            request: Request<tonic::Streaming<DiscoveryRequest>>,
        ) -> std::result::Result<Response<Self::StreamAggregatedResourcesStream>, Status> {
            if let Some(expected) = &self.expected_auth {
                let got = request
                    .metadata()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok());
                if got != Some(expected.as_str()) {
                    return Err(Status::unauthenticated(
                        "missing or unexpected authorization",
                    ));
                }
            }
            let mut inbound = request.into_inner();
            let padding = self.response_padding;

            let outbound = async_stream::try_stream! {
                while let Some(req) = inbound.next().await {
                    let req = req?;
                    let resources = if padding > 0 {
                        vec![envoy_types::pb::google::protobuf::Any {
                            type_url: req.type_url.clone(),
                            value: vec![0u8; padding],
                        }]
                    } else {
                        vec![]
                    };
                    let response = DiscoveryResponse {
                        version_info: "1".to_string(),
                        type_url: req.type_url.clone(),
                        nonce: "nonce-1".to_string(),
                        resources,
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

    async fn start_mock_server(expected_auth: Option<&str>) -> SocketAddr {
        spawn_mock_server(expected_auth, 0).await
    }

    /// Start a server that pads every response to just over `response_padding` bytes.
    async fn start_padded_mock_server(response_padding: usize) -> SocketAddr {
        spawn_mock_server(None, response_padding).await
    }

    async fn spawn_mock_server(expected_auth: Option<&str>, response_padding: usize) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = MockAdsServer {
            expected_auth: expected_auth.map(str::to_owned),
            response_padding,
        };

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(AggregatedDiscoveryServiceServer::new(server))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        addr
    }

    fn discovery_request() -> Bytes {
        DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            ..Default::default()
        }
        .encode_to_vec()
        .into()
    }

    #[derive(Debug)]
    struct MockCreds {
        pairs: Vec<(String, String)>,
        requires_secure: bool,
    }

    #[tonic::async_trait]
    impl TonicCallCredentials for MockCreds {
        async fn get_request_metadata(
            &self,
            metadata: &mut tonic::metadata::MetadataMap,
        ) -> std::result::Result<(), tonic::Status> {
            for (name, value) in &self.pairs {
                let key = tonic::metadata::AsciiMetadataKey::from_bytes(name.as_bytes())
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                let val = tonic::metadata::AsciiMetadataValue::try_from(value)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                metadata.insert(key, val);
            }
            Ok(())
        }
        fn requires_secure_transport(&self) -> bool {
            self.requires_secure
        }
    }

    #[tokio::test]
    async fn call_creds_attach_metadata() {
        let addr = start_mock_server(Some("Bearer test-token")).await;
        let creds = Arc::new(MockCreds {
            pairs: vec![("authorization".into(), "Bearer test-token".into())],
            requires_secure: false,
        });
        let transport = TonicTransportBuilder::new()
            .with_call_credentials(creds)
            .build(&ServerConfig::new(format!("http://{addr}")))
            .await
            .unwrap();
        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            ..Default::default()
        };
        let request_bytes: Bytes = request.encode_to_vec().into();
        let (mut tx, mut rx) = transport.new_stream().await.unwrap();
        tx.send(request_bytes).await.unwrap();
        let response = rx.recv().await.unwrap().unwrap();
        let response = DiscoveryResponse::decode(response).unwrap();
        assert_eq!(response.version_info, "1");
    }

    #[tokio::test]
    async fn from_channel_connects_and_streams() {
        let addr = start_mock_server(None).await;
        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect_lazy();
        let transport = TonicTransport::from_channel(channel);
        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            ..Default::default()
        };
        let request_bytes: Bytes = request.encode_to_vec().into();
        let (mut tx, mut rx) = transport.new_stream().await.unwrap();
        tx.send(request_bytes).await.unwrap();
        let response = rx.recv().await.unwrap().unwrap();
        let response = DiscoveryResponse::decode(response).unwrap();
        assert_eq!(response.version_info, "1");
    }

    /// Larger than tonic's 4 MiB default decoding limit.
    const OVERSIZED_RESPONSE: usize = 5 * 1024 * 1024;

    #[tokio::test]
    async fn oversized_response_fails_at_default_limit() {
        let addr = start_padded_mock_server(OVERSIZED_RESPONSE).await;
        let transport = TonicTransportBuilder::new()
            .build(&ServerConfig::new(format!("http://{addr}")))
            .await
            .unwrap();
        let (mut tx, mut rx) = transport.new_stream().await.unwrap();
        tx.send(discovery_request()).await.unwrap();
        let err = rx.recv().await.unwrap_err();
        let Error::Stream(status) = err else {
            panic!("expected a stream error, got {err:?}");
        };
        assert_eq!(status.code(), tonic::Code::OutOfRange);
    }

    #[tokio::test]
    async fn raised_limit_accepts_oversized_response() {
        let addr = start_padded_mock_server(OVERSIZED_RESPONSE).await;
        let transport = TonicTransportBuilder::new()
            .with_max_decoding_message_size(OVERSIZED_RESPONSE * 2)
            .build(&ServerConfig::new(format!("http://{addr}")))
            .await
            .unwrap();
        let (mut tx, mut rx) = transport.new_stream().await.unwrap();
        tx.send(discovery_request()).await.unwrap();
        let response = rx.recv().await.unwrap().unwrap();
        let response = DiscoveryResponse::decode(response).unwrap();
        assert_eq!(response.resources[0].value.len(), OVERSIZED_RESPONSE);
    }

    #[tokio::test]
    async fn call_creds_require_secure_transport() {
        // The check runs before connecting, so no server is needed.
        let err = TonicTransportBuilder::new()
            .with_call_credentials(Arc::new(MockCreds {
                pairs: vec![],
                requires_secure: true,
            }))
            .build(&ServerConfig::new("http://127.0.0.1:1"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::CallCredentials(_)));
    }

    #[cfg(feature = "_tonic-tls-any")]
    #[tokio::test]
    async fn tls_config_and_plaintext_uri_are_rejected() {
        // A URI that keeps a non-`https` scheme stays plaintext despite the TLS
        // config, so the build fails and the call credentials below stay put.
        for uri in [
            "http://127.0.0.1:1",
            "unix:///etc/istio/proxy/XDS",
            "foo://127.0.0.1:1",
        ] {
            let err = TonicTransportBuilder::new()
                .with_tls_config(tonic::transport::ClientTlsConfig::new())
                .with_call_credentials(Arc::new(MockCreds {
                    pairs: vec![],
                    requires_secure: true,
                }))
                .build(&ServerConfig::new(uri))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("connects in plaintext"), "{uri}");
        }
    }

    #[cfg(feature = "_tonic-tls-any")]
    #[tokio::test]
    async fn tls_config_factory_is_called_for_each_transport_build() {
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = calls.clone();
        let builder = TonicTransportBuilder::new().with_tls_config_factory(move |_| {
            let call = factory_calls.fetch_add(1, Ordering::SeqCst) + 1;
            Err(Error::TlsConfig(Box::new(std::io::Error::other(format!(
                "snapshot {call}"
            )))))
        });
        let server = ServerConfig::new("xds.example.com:443");

        for expected_call in 1..=2 {
            let error = builder.build(&server).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("snapshot {expected_call}"))
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[cfg(feature = "_tonic-tls-any")]
    #[tokio::test]
    async fn invalid_tls_material_is_a_tls_config_error() {
        let tls_config = tonic::transport::ClientTlsConfig::new().identity(
            tonic::transport::Identity::from_pem(b"not a certificate", b"not a private key"),
        );
        let error = TonicTransportBuilder::new()
            .with_tls_config(tls_config)
            .build(&ServerConfig::new("xds.example.com:443"))
            .await
            .unwrap_err();

        assert!(matches!(&error, Error::TlsConfig(_)));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn tls_uri_without_a_tls_config_is_rejected() {
        // Without a TLS backend tonic connects to this in the clear.
        let err = TonicTransportBuilder::new()
            .build(&ServerConfig::new("https://127.0.0.1:1"))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("requires TLS but no TLS config is set")
        );
    }

    #[test]
    fn ensure_secure_server_uri_adds_scheme_only_when_needed() {
        assert_eq!(
            TonicTransportBuilder::ensure_secure_server_uri(
                "trafficdirector.googleapis.com:443",
                true
            ),
            "https://trafficdirector.googleapis.com:443",
        );
        assert_eq!(
            TonicTransportBuilder::ensure_secure_server_uri("https://xds.example.com:443", true),
            "https://xds.example.com:443"
        );
        assert_eq!(
            TonicTransportBuilder::ensure_secure_server_uri("unix:///etc/istio/proxy/XDS", true),
            "unix:///etc/istio/proxy/XDS"
        );
        assert_eq!(
            TonicTransportBuilder::ensure_secure_server_uri("127.0.0.1:18000", false),
            "127.0.0.1:18000"
        );
    }

    #[tokio::test]
    async fn test_tonic_transport_connect_and_stream() {
        let addr = start_mock_server(None).await;
        let uri = format!("http://{addr}");

        let transport = TonicTransport::connect(&uri).await.unwrap();

        let request = DiscoveryRequest {
            type_url: "type.googleapis.com/envoy.config.listener.v3.Listener".to_string(),
            resource_names: vec!["listener-1".to_string()],
            ..Default::default()
        };
        let request_bytes: Bytes = request.encode_to_vec().into();

        let (mut tx, mut rx) = transport.new_stream().await.unwrap();
        tx.send(request_bytes).await.unwrap();

        let response_bytes = rx.recv().await.unwrap().unwrap();
        let response = DiscoveryResponse::decode(response_bytes).unwrap();

        assert_eq!(response.version_info, "1");
        assert_eq!(
            response.type_url,
            "type.googleapis.com/envoy.config.listener.v3.Listener"
        );
        assert_eq!(response.nonce, "nonce-1");
    }
}
