/*
 *
 * Copyright 2026 gRPC authors.
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

//! `grpc` crate based transport implementation for `xds-client`.
//!
//! This transport uses the `grpc` crate's [`Channel`] and low-level streams
//! to send and receive raw bytes.

// TODO: remove once XdsClient is wired up in grpc-xds.
#![allow(dead_code)]

use std::sync::Arc;

use bytes::Buf;
use bytes::Bytes;
use grpc::client::CallOptions;
use grpc::client::Channel;
use grpc::client::DynRecvStream;
use grpc::client::DynSendStream;
use grpc::client::Invoke as _;
use grpc::client::RecvStream as _;
use grpc::client::RequestHeaders;
use grpc::client::ResponseStreamItem;
use grpc::client::SendOptions;
use grpc::client::SendStream as _;
use grpc::core::RecvMessage;
use grpc::core::SendMessage;
use grpc::credentials::ChannelCredentials;
use grpc::credentials::LocalChannelCredentials;
use xds_client::Error;
use xds_client::Result;
use xds_client::ServerConfig;
use xds_client::Transport;
use xds_client::TransportBuilder;
use xds_client::TransportReceiver;
use xds_client::TransportSender;

/// The gRPC path for the ADS StreamAggregatedResources RPC.
const ADS_PATH: &str =
    "/envoy.service.discovery.v3.AggregatedDiscoveryService/StreamAggregatedResources";

/// Factory for creating gRPC-based transports for `xds-client`.
#[derive(Clone)]
pub(crate) struct GrpcTransportBuilder {
    credentials: Arc<dyn ChannelCredentials>,
}

impl std::fmt::Debug for GrpcTransportBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTransportBuilder").finish()
    }
}

impl Default for GrpcTransportBuilder {
    fn default() -> Self {
        Self {
            credentials: Arc::new(LocalChannelCredentials::new()),
        }
    }
}

impl GrpcTransportBuilder {
    /// Creates a new transport builder with default credentials.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Sets the channel credentials to use when building channels.
    pub(crate) fn with_credentials(mut self, credentials: Arc<dyn ChannelCredentials>) -> Self {
        self.credentials = credentials;
        self
    }
}

impl TransportBuilder for GrpcTransportBuilder {
    type Transport = GrpcTransport;

    async fn build(&self, server: &ServerConfig) -> Result<Self::Transport> {
        let channel = Channel::builder(server.uri(), self.credentials.clone()).build();
        Ok(GrpcTransport::from_channel(channel))
    }
}

/// A gRPC transport connected to an xDS management server using the `grpc` crate.
#[derive(Clone)]
pub(crate) struct GrpcTransport {
    channel: Channel,
}

impl std::fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTransport").finish()
    }
}

impl GrpcTransport {
    /// Creates a transport wrapping an existing `grpc::client::Channel`.
    pub(crate) fn from_channel(channel: Channel) -> Self {
        Self { channel }
    }
}

impl Transport for GrpcTransport {
    type Sender = GrpcAdsSender;
    type Receiver = GrpcAdsReceiver;

    async fn new_stream(&self) -> Result<(Self::Sender, Self::Receiver)> {
        let headers = RequestHeaders::new().with_method_name(ADS_PATH);
        let (send_stream, recv_stream) = self.channel.invoke(headers, CallOptions::new()).await;

        Ok((GrpcAdsSender(send_stream), GrpcAdsReceiver(recv_stream)))
    }
}

/// Sending half of a `GrpcTransport` ADS stream.
pub(crate) struct GrpcAdsSender(Box<dyn DynSendStream>);

impl std::fmt::Debug for GrpcAdsSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcAdsSender").finish()
    }
}

impl TransportSender for GrpcAdsSender {
    async fn send(&mut self, request: Bytes) -> Result<()> {
        let msg = RawBytes(request);
        self.0
            .send(&msg, SendOptions::new())
            .await
            .map_err(|_| Error::StreamClosed)?;
        Ok(())
    }
}

/// Receiving half of a `GrpcTransport` ADS stream.
pub(crate) struct GrpcAdsReceiver(Box<dyn DynRecvStream>);

impl std::fmt::Debug for GrpcAdsReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcAdsReceiver").finish()
    }
}

impl TransportReceiver for GrpcAdsReceiver {
    async fn recv(&mut self) -> Result<Option<Bytes>> {
        loop {
            let mut msg_buf = RawBytesBuffer::default();
            let item = self.0.recv(&mut msg_buf).await;
            match item {
                ResponseStreamItem::Headers(_) => {
                    // Ignore response headers, wait for messages or trailers.
                    continue;
                }
                ResponseStreamItem::Message => {
                    return Ok(Some(msg_buf.0));
                }
                ResponseStreamItem::Trailers(trailers) => {
                    if let Err(err) = trailers.into_status() {
                        return Err(Error::Transport(Box::new(err)));
                    }
                    return Ok(None);
                }
                ResponseStreamItem::StreamClosed => {
                    return Ok(None);
                }
            }
        }
    }
}

struct RawBytes(Bytes);

impl SendMessage for RawBytes {
    fn encode(&self) -> std::result::Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(self.0.clone()))
    }
}

#[derive(Default)]
struct RawBytesBuffer(Bytes);

impl RecvMessage for RawBytesBuffer {
    fn decode(&mut self, data: &mut dyn Buf) -> std::result::Result<(), String> {
        self.0 = data.copy_to_bytes(data.remaining());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grpc::credentials::LocalChannelCredentials;
    use protobuf::Parse as _;
    use protobuf::Serialize as _;
    use xds_client::ClientConfig;
    use xds_client::Node;
    use xds_client::ProstCodec;
    use xds_client::ResourceEvent;
    use xds_client::TokioRuntime;
    use xds_client::XdsClient;
    use xds_test_util::XdsTestControlPlaneService;
    use xds_test_util::config::AdsTypeUrl;
    use xds_test_util::config::build_inline_listener;

    use super::*;
    use crate::generated::envoy::service::discovery::v3::DiscoveryRequest;
    use crate::generated::envoy::service::discovery::v3::DiscoveryResponse;
    use crate::resource::ListenerResource;

    #[tokio::test]
    async fn test_builder_build() {
        let builder =
            GrpcTransportBuilder::new().with_credentials(Arc::new(LocalChannelCredentials::new()));

        let server_config = ServerConfig::new("dns:///localhost:50051");
        let _transport = builder.build(&server_config).await.unwrap();
    }

    #[test]
    fn test_raw_bytes_encode_decode() {
        let original = Bytes::from_static(b"hello xds");
        let raw = RawBytes(original.clone());
        let mut buf = raw.encode().unwrap();

        let mut decoded = RawBytesBuffer::default();
        decoded.decode(&mut buf).unwrap();

        assert_eq!(decoded.0, original);
    }

    #[tokio::test]
    async fn test_grpc_transport_connect_and_stream() {
        let running_cp = XdsTestControlPlaneService::new()
            .start()
            .await
            .expect("start control plane");

        let addr = running_cp.addr();
        let server_config = ServerConfig::new(format!("dns:///{addr}"));
        let transport = GrpcTransportBuilder::new()
            .build(&server_config)
            .await
            .expect("build transport");

        let (mut tx, mut rx) = transport.new_stream().await.expect("new stream");

        let mut request = DiscoveryRequest::new();
        request.set_type_url(AdsTypeUrl::Lds.to_string());
        request.resource_names_mut().push("listener-1");
        tx.send(request.serialize().expect("serialize").into())
            .await
            .expect("send discovery request");

        let response_bytes = rx
            .recv()
            .await
            .expect("recv result")
            .expect("response message");
        let response = DiscoveryResponse::parse(&response_bytes).expect("decode response");
        assert_eq!(
            response.type_url().to_str().unwrap(),
            AdsTypeUrl::Lds.to_string()
        );
    }

    #[tokio::test]
    async fn watch_with_grpc_transport() {
        let control_plane = XdsTestControlPlaneService::new();
        let running_cp = control_plane.start().await.expect("start control plane");

        let mut listeners = HashMap::new();
        listeners.insert(
            "listener-1".to_string(),
            build_inline_listener("listener-1", "cluster-1"),
        );
        control_plane.set_xds_config(&AdsTypeUrl::Lds, listeners);

        let config = ClientConfig::new(
            Node::new("test", "0"),
            format!("dns:///{}", running_cp.addr()),
        );
        let client =
            XdsClient::builder(config, GrpcTransportBuilder::new(), ProstCodec, TokioRuntime)
                .build();

        let mut watcher = client.watch::<ListenerResource>("listener-1").await;
        let event = watcher.next().await.expect("watcher event");
        let ResourceEvent::ResourceChanged {
            result: Ok(listener),
            done: _done,
        } = event
        else {
            panic!("expected ResourceChanged Ok, got {event:?}");
        };
        assert_eq!(listener.name, "listener-1");
    }
}
