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

//! End-to-end tests using [`RustlsChannelCredentials`] with a gRPC [`Channel`]
//! talking to a TLS-enabled tonic server.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Once;

use bytes::Buf;
use bytes::Bytes;
use grpc::__unstable::client::name_resolution::TCP_IP_NETWORK_TYPE;
use grpc::StatusError;
use grpc::async_trait;
use grpc::client::CallOptions;
use grpc::client::Channel;
use grpc::client::Invoke as _;
use grpc::client::RecvStream as _;
use grpc::client::RequestHeaders;
use grpc::client::ResponseHeaders;
use grpc::client::ResponseStreamItem;
use grpc::client::SendOptions;
use grpc::client::SendStream as _;
use grpc::client::Trailers;
use grpc::core::RecvMessage;
use grpc::core::SendMessage;
use grpc::credentials::CompositeChannelCredentials;
use grpc::credentials::SecurityLevel;
use grpc::credentials::call::CallCredentials;
use grpc::credentials::call::CallDetails;
use grpc::credentials::call::ClientConnectionSecurityInfo;
use grpc::metadata::AsciiMetadataKey;
use grpc::metadata::MetadataMap;
use grpc_rustls::RootCertificates;
use grpc_rustls::StaticProvider;
use grpc_rustls::client::ClientTlsConfig;
use grpc_rustls::client::RustlsChannelCredentials;
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_stream::Stream;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

#[allow(unreachable_pub, missing_docs, missing_debug_implementations)]
mod echo_pb {
    include!("generated/grpc_examples_echo.rs");
}

use echo_pb::EchoRequest;
use echo_pb::EchoResponse;
use echo_pb::echo_server::Echo;
use echo_pb::echo_server::EchoServer;

static INIT: Once = Once::new();

fn init_provider() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug)]
struct MockCallCredentials {
    metadata: Vec<(&'static str, &'static str)>,
    min_security_level: SecurityLevel,
}

#[async_trait]
impl CallCredentials for MockCallCredentials {
    async fn get_metadata(
        &self,
        _call_details: &CallDetails,
        _auth_info: &ClientConnectionSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), StatusError> {
        for (key, val) in &self.metadata {
            metadata.insert(
                key.parse::<AsciiMetadataKey>().unwrap(),
                val.parse().unwrap(),
            );
        }
        Ok(())
    }

    fn minimum_channel_security_level(&self) -> SecurityLevel {
        self.min_security_level
    }
}

#[tokio::test]
async fn grpc_invoke_tonic_unary_tls() {
    init_provider();

    let certs_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("examples/data/tls");

    let server_cert = fs::read(certs_path.join("server.pem")).expect("failed to read server.pem");
    let server_key = fs::read(certs_path.join("server.key")).expect("failed to read server.key");
    let ca_cert = fs::read(certs_path.join("ca.pem")).expect("failed to read ca.pem");

    let identity = tonic::transport::Identity::from_pem(server_cert, server_key);
    let tls_config = tonic::transport::ServerTlsConfig::new().identity(identity);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_notify_copy = shutdown_notify.clone();

    // Spawn a task for the server.
    let server_handle = tokio::spawn(async move {
        let svc = EchoServer::new(EchoService {});
        let _ = Server::builder()
            .tls_config(tls_config)
            .expect("failed to set tls config")
            .add_service(svc)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    // Create the channel.
    let root_certs = RootCertificates::from_pem(ca_cert);
    let root_provider = StaticProvider::new(root_certs);
    let config = ClientTlsConfig::new().with_root_certificates_provider(root_provider);
    let creds = RustlsChannelCredentials::new(config).unwrap();
    let call_creds = Arc::new(MockCallCredentials {
        metadata: vec![("x-test-metadata", "test-value")],
        min_security_level: SecurityLevel::PrivacyAndIntegrity,
    });
    let composite_creds = CompositeChannelCredentials::new(creds, call_creds);

    let target = format!("dns:///{addr}");
    let channel = Channel::builder(&target, Arc::new(composite_creds)).build();

    let (headers, resp, trailers) = perform_unary_echo(&channel, "hello interop tls").await;

    assert_eq!(
        headers.metadata().get("x-test-metadata-echo").unwrap(),
        "test-value"
    );
    assert_eq!(resp.message, "hello interop tls");

    let connection_info = headers.connection_info();
    assert_eq!(
        connection_info.local_address().network_type,
        TCP_IP_NETWORK_TYPE
    );
    let local_addr: SocketAddr = connection_info.local_address().address.parse().unwrap();
    assert_eq!(local_addr.ip(), addr.ip());
    assert_eq!(
        connection_info.remote_address().network_type,
        TCP_IP_NETWORK_TYPE
    );
    assert_eq!(
        connection_info.remote_address().address.to_string(),
        addr.to_string()
    );
    assert_eq!(connection_info.security_info().security_protocol(), "tls");

    assert!(
        trailers.connection_info().is_none(),
        "trailers should not contain connection_info when headers were present; had {:?}",
        trailers.connection_info().as_ref().unwrap()
    );

    assert!(
        trailers.status().is_ok(),
        "RPC failed: {:?}",
        trailers.status()
    );

    shutdown_notify.notify_one();
    server_handle.await.unwrap();
}

async fn perform_unary_echo(
    channel: &Channel,
    message: &str,
) -> (ResponseHeaders, EchoResponse, Trailers) {
    let (mut tx, mut rx) = channel
        .invoke(
            RequestHeaders::new().with_method_name("/grpc.examples.echo.Echo/UnaryEcho"),
            CallOptions::default(),
        )
        .await;

    let req = WrappedEchoRequest(EchoRequest {
        message: message.into(),
    });
    tx.send(&req, SendOptions::new().with_final_msg(true))
        .await
        .unwrap();

    let mut resp = WrappedEchoResponse(EchoResponse::default());

    let ResponseStreamItem::Headers(headers) = rx.recv(&mut resp).await else {
        panic!("Expected Headers first");
    };

    let ResponseStreamItem::Message = rx.recv(&mut resp).await else {
        panic!("Expected Message after Headers");
    };
    let echo_resp = std::mem::take(&mut resp.0);

    let ResponseStreamItem::Trailers(trailers) = rx.recv(&mut resp).await else {
        panic!("Expected Trailers, got StreamClosed or other item");
    };

    (headers, echo_resp, trailers)
}

struct WrappedEchoRequest(EchoRequest);
struct WrappedEchoResponse(EchoResponse);

impl SendMessage for WrappedEchoRequest {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(Bytes::from(self.0.encode_to_vec())))
    }
}

impl RecvMessage for WrappedEchoResponse {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String> {
        let buf = data.copy_to_bytes(data.remaining());
        self.0 = EchoResponse::decode(buf).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[derive(Debug)]
struct EchoService {}

type EchoStream = Pin<Box<dyn Stream<Item = Result<EchoResponse, tonic::Status>> + Send + 'static>>;

#[async_trait]
impl Echo for EchoService {
    async fn unary_echo(
        &self,
        request: tonic::Request<EchoRequest>,
    ) -> Result<tonic::Response<EchoResponse>, tonic::Status> {
        let metadata = request.metadata().clone();
        let message = request.into_inner().message;
        let mut response = tonic::Response::new(EchoResponse { message });
        if let Some(val) = metadata.get("x-test-metadata") {
            response
                .metadata_mut()
                .insert("x-test-metadata-echo", val.clone());
        }
        Ok(response)
    }

    type ServerStreamingEchoStream = EchoStream;

    async fn server_streaming_echo(
        &self,
        _: tonic::Request<EchoRequest>,
    ) -> Result<tonic::Response<Self::ServerStreamingEchoStream>, tonic::Status> {
        unimplemented!()
    }

    async fn client_streaming_echo(
        &self,
        _: tonic::Request<tonic::Streaming<EchoRequest>>,
    ) -> Result<tonic::Response<EchoResponse>, tonic::Status> {
        unimplemented!()
    }

    type BidirectionalStreamingEchoStream = EchoStream;

    async fn bidirectional_streaming_echo(
        &self,
        _: tonic::Request<tonic::Streaming<EchoRequest>>,
    ) -> Result<tonic::Response<Self::BidirectionalStreamingEchoStream>, tonic::Status> {
        unimplemented!()
    }
}
