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

use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use bytes::Buf;
use bytes::Bytes;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::net::UnixListener;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::Response;
use tonic::async_trait;
use tonic::metadata::MetadataMap;
use tonic::transport::Server;
use tonic_prost::prost::Message as ProstMessage;

use crate::Status;
use crate::StatusCode;
use crate::client::CallOptions;
use crate::client::Channel;
use crate::client::Invoke as _;
use crate::client::RecvStream as _;
use crate::client::SendOptions;
use crate::client::SendStream as _;
use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::client::transport::SecurityOpts;
use crate::client::transport::TransportOptions;
use crate::client::transport::registry::GLOBAL_TRANSPORT_REGISTRY;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::ResponseHeaders;
use crate::core::SendMessage;
use crate::core::Trailers;
use crate::credentials::CompositeChannelCredentials;
use crate::credentials::InsecureChannelCredentials;
use crate::credentials::LocalChannelCredentials;
use crate::credentials::SecurityLevel;
use crate::credentials::call::CallCredentials;
use crate::credentials::call::CallDetails;
use crate::credentials::call::ClientConnectionSecurityInfo;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::common::Authority;
use crate::credentials::rustls::RootCertificates;
use crate::credentials::rustls::StaticProvider;
use crate::credentials::rustls::client::ClientTlsConfig;
use crate::credentials::rustls::client::RustlsClientTlsCredendials;
use crate::echo_pb::EchoRequest;
use crate::echo_pb::EchoResponse;
use crate::echo_pb::echo_server::Echo;
use crate::echo_pb::echo_server::EchoServer;
use crate::rt::GrpcRuntime;
use crate::rt::tokio::TokioRuntime;

#[derive(Debug)]
struct MockCallCredentials {
    metadata: Vec<(&'static str, &'static str)>,
    min_security_level: SecurityLevel,
    should_fail: Option<crate::Status>,
}

#[async_trait]
impl CallCredentials for MockCallCredentials {
    async fn get_metadata(
        &self,
        _call_details: &CallDetails,
        _auth_info: &ClientConnectionSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), crate::Status> {
        if let Some(status) = &self.should_fail {
            return Err(status.clone());
        }
        for (key, val) in &self.metadata {
            metadata.insert(
                key.parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                    .unwrap(),
                val.parse().unwrap(),
            );
        }
        Ok(())
    }

    fn minimum_channel_security_level(&self) -> SecurityLevel {
        self.min_security_level
    }
}

const DEFAULT_TEST_DURATION: Duration = Duration::from_secs(10);
const DEFAULT_TEST_SHORT_DURATION: Duration = Duration::from_millis(10);

// Tests the tonic transport by creating a bi-di stream with a tonic server.
#[tokio::test]
pub(crate) async fn tonic_transport_rpc() {
    super::reg();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap(); // get the assigned address
    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_notify_copy = shutdown_notify.clone();
    println!("EchoServer listening on: {addr}");
    let server_handle = tokio::spawn(async move {
        let echo_server = EchoService {};
        let svc = EchoServer::new(echo_server);
        let _ = Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    let builder = GLOBAL_TRANSPORT_REGISTRY
        .get_transport(TCP_IP_NETWORK_TYPE)
        .unwrap();
    let config = Arc::new(TransportOptions::default());
    let securty_opts = SecurityOpts {
        credentials: InsecureChannelCredentials::new_arc(),
        authority: Authority::new("localhost".to_string(), None),
        handshake_info: ClientHandshakeInfo::default(),
    };
    let (conn, _sec_info, mut disconnection_listener) = builder
        .dyn_connect(
            addr.to_string(),
            GrpcRuntime::new(TokioRuntime::default()),
            &securty_opts,
            &config,
        )
        .await
        .unwrap();

    let (mut tx, mut rx) = conn
        .dyn_invoke(
            RequestHeaders::new()
                .with_method_name("/grpc.examples.echo.Echo/BidirectionalStreamingEcho"),
            CallOptions::default(),
        )
        .await;

    // Spawn a sender task
    let client_handle = tokio::spawn(async move {
        let mut dummy_msg = WrappedEchoResponse(EchoResponse { message: "".into() });
        match rx.next(&mut dummy_msg).await {
            ClientResponseStreamItem::Headers(_) => {
                println!("Got headers");
            }
            item => panic!("Expected headers, got {:?}", item),
        }

        for i in 0..5 {
            let message = format!("message {i}");
            let request = EchoRequest {
                message: message.clone(),
            };

            let req = WrappedEchoRequest(request);

            println!("Sent request: {:?}", req.0);
            assert!(
                tx.send(&req, SendOptions::default()).await.is_ok(),
                "Receiver dropped"
            );

            // Wait for the reply
            let mut recv_msg = WrappedEchoResponse(EchoResponse { message: "".into() });
            match rx.next(&mut recv_msg).await {
                ClientResponseStreamItem::Message(()) => {
                    let echo_response = recv_msg.0;
                    println!("Got response: {echo_response:?}");
                    assert_eq!(echo_response.message, message);
                }
                item => panic!("Expected message, got {:?}", item),
            }
        }
    });

    client_handle.await.unwrap();
    // The connection should break only after the server is stopped.
    assert_eq!(
        disconnection_listener.try_recv(),
        Err(oneshot::error::TryRecvError::Empty),
    );
    shutdown_notify.notify_waiters();
    let res = timeout(DEFAULT_TEST_DURATION, disconnection_listener)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res, Ok(()));
    server_handle.await.unwrap();
}

#[tokio::test]
async fn grpc_invoke_tonic_unary() {
    // Register DNS & Tonic.
    super::reg();
    crate::client::name_resolution::dns::reg();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_notify_copy = shutdown_notify.clone();

    // Spawn a task for the server.
    let server_handle = tokio::spawn(async move {
        let echo_server = EchoService {};
        let svc = EchoServer::new(echo_server);
        let _ = Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    // Create the channel.
    let target = format!("dns:///{}", addr);
    let channel = Channel::new(
        &target,
        InsecureChannelCredentials::new_arc(),
        Default::default(),
    );

    let (_, resp, trailers) = perform_unary_echo(&channel, "hello interop").await;
    assert_eq!(resp.message, "hello interop");

    assert_eq!(
        trailers.status().code(),
        StatusCode::Ok,
        "RPC failed: {:?}",
        trailers.status()
    );

    shutdown_notify.notify_one();
    server_handle.await.unwrap();
}

#[tokio::test]
async fn grpc_invoke_tonic_unix() {
    super::reg();
    crate::client::name_resolution::unix::reg();

    let dir = tempdir().expect("failed to create temp dir");

    // Absolute path
    {
        println!("Testing absolute path unix socket...");
        let socket_path = dir.path().join("absolute.sock");
        assert!(socket_path.is_absolute());
        let listener = UnixListener::bind(&socket_path).unwrap();
        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_notify_copy = shutdown_notify.clone();

        let server_handle = tokio::spawn(async move {
            let echo_server = EchoService {};
            let svc = EchoServer::new(echo_server);
            let _ = Server::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(
                    UnixListenerStream::new(listener),
                    shutdown_notify_copy.notified(),
                )
                .await;
        });

        let target = format!("unix://{}", socket_path.to_str().unwrap());
        let channel = Channel::new(
            &target,
            LocalChannelCredentials::new_arc(),
            Default::default(),
        );

        let (_, resp, trailers) = perform_unary_echo(&channel, "hello absolute unix").await;
        assert_eq!(resp.message, "hello absolute unix");
        assert_eq!(trailers.status().code(), StatusCode::Ok);

        shutdown_notify.notify_one();
        server_handle.await.unwrap();
        println!("Absolute path test passed.");
    }

    // Relative path
    {
        println!("Testing relative path unix socket...");
        let socket_name = "relative.sock";
        let socket_path = dir.path().join(socket_name);
        let listener = UnixListener::bind(&socket_path).unwrap();

        let current_dir = std::env::current_dir().expect("failed to fetch current directory");

        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_notify_copy = shutdown_notify.clone();

        let server_handle = tokio::spawn(async move {
            let echo_server = EchoService {};
            let svc = EchoServer::new(echo_server);
            let _ = Server::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(
                    UnixListenerStream::new(listener),
                    shutdown_notify_copy.notified(),
                )
                .await;
        });

        let relative_path = get_relative_path(&socket_path, &current_dir)
            .expect("current directory and temp directory don't share a common ancestor");
        let target = format!("unix:{}", relative_path.display());
        println!("grpc target: {}", target);
        let channel = Channel::new(
            &target,
            InsecureChannelCredentials::new_arc(),
            Default::default(),
        );

        let (_, resp, trailers) = perform_unary_echo(&channel, "hello relative unix").await;
        assert_eq!(resp.message, "hello relative unix");
        assert_eq!(trailers.status().code(), StatusCode::Ok);

        shutdown_notify.notify_one();
        server_handle.await.unwrap();
        std::env::set_current_dir(current_dir).unwrap();
        println!("Relative path test passed.");
    }

    // Abstract unix
    #[cfg(target_os = "linux")]
    {
        println!("Testing abstract unix socket...");
        let abstract_path = format!("grpc-test-abstract-socket-{}", rand::random::<u64>());
        let listener = UnixListener::bind(format!("\0{}", abstract_path)).unwrap();
        let shutdown_notify = Arc::new(Notify::new());
        let shutdown_notify_copy = shutdown_notify.clone();

        let server_handle = tokio::spawn(async move {
            let echo_server = EchoService {};
            let svc = EchoServer::new(echo_server);
            let _ = Server::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(
                    UnixListenerStream::new(listener),
                    shutdown_notify_copy.notified(),
                )
                .await;
        });

        let target = format!("unix-abstract:{}", abstract_path);
        let channel = Channel::new(
            &target,
            InsecureChannelCredentials::new_arc(),
            Default::default(),
        );

        let (_, resp, trailers) = perform_unary_echo(&channel, "hello abstract unix").await;
        assert_eq!(resp.message, "hello abstract unix");
        assert_eq!(trailers.status().code(), StatusCode::Ok);

        shutdown_notify.notify_one();
        server_handle.await.unwrap();
        println!("Abstract unix test passed.");
    }
}

static INIT: Once = Once::new();

fn init_provider() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn grpc_invoke_tonic_unary_tls() {
    init_provider();
    // Register DNS & Tonic.
    super::reg();
    crate::client::name_resolution::dns::reg();

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
        let echo_server = EchoService {};
        let svc = EchoServer::new(echo_server);
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
    let creds = RustlsClientTlsCredendials::new(config).unwrap();
    let call_creds = Arc::new(MockCallCredentials {
        metadata: vec![("x-test-metadata", "test-value")],
        min_security_level: SecurityLevel::PrivacyAndIntegrity,
        should_fail: None,
    });
    let composite_creds = CompositeChannelCredentials::new(creds, call_creds).unwrap();

    let target = format!("dns:///{}", addr);
    let channel = Channel::new(&target, Arc::new(composite_creds), Default::default());

    let (headers, resp, trilers) = perform_unary_echo(&channel, "hello interop tls").await;
    assert_eq!(
        headers.metadata().get("x-test-metadata-echo").unwrap(),
        "test-value"
    );
    assert_eq!(resp.message, "hello interop tls");

    assert_eq!(
        trilers.status().code(),
        StatusCode::Ok,
        "RPC failed: {:?}",
        trilers.status()
    );

    shutdown_notify.notify_one();
    server_handle.await.unwrap();
}

#[tokio::test]
async fn grpc_invoke_failure_cases() {
    super::reg();
    crate::client::name_resolution::dns::reg();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_notify_copy = shutdown_notify.clone();

    tokio::spawn(async move {
        let echo_server = EchoService {};
        let svc = EchoServer::new(echo_server);
        let _ = Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    let target = format!("dns:///{}", addr);

    // Security level mismatch (MockCallCredentials requires PrivacyAndIntegrity,
    // but LocalChannelCredentials provides NoSecurity over TCP).
    {
        let creds = LocalChannelCredentials::new();
        let call_creds = Arc::new(MockCallCredentials {
            metadata: vec![],
            min_security_level: SecurityLevel::PrivacyAndIntegrity,
            should_fail: None,
        });
        let composite_creds = CompositeChannelCredentials::new(creds, call_creds).unwrap();
        let channel = Channel::new(&target, Arc::new(composite_creds), Default::default());

        let trailers = perform_unary_echo_failure(&channel).await;
        assert_eq!(trailers.status().code(), StatusCode::Unauthenticated);
    }

    // Call credentials return error
    {
        let creds = LocalChannelCredentials::new();
        let call_creds = Arc::new(MockCallCredentials {
            metadata: vec![],
            min_security_level: SecurityLevel::NoSecurity,
            should_fail: Some(crate::Status::new(
                StatusCode::PermissionDenied,
                "test message",
            )),
        });
        let composite_creds = CompositeChannelCredentials::new(creds, call_creds).unwrap();
        let channel = Channel::new(&target, Arc::new(composite_creds), Default::default());

        let trailers = perform_unary_echo_failure(&channel).await;
        assert_eq!(trailers.status().code(), StatusCode::PermissionDenied);
        assert!(trailers.status().message().contains("test message"));
    }

    // Call credentials return restricted control plane code (mapped to Internal)
    {
        let creds = LocalChannelCredentials::new();
        let call_creds = Arc::new(MockCallCredentials {
            metadata: vec![],
            min_security_level: SecurityLevel::NoSecurity,
            should_fail: Some(Status::new(StatusCode::InvalidArgument, "test message")),
        });
        let composite_creds = CompositeChannelCredentials::new(creds, call_creds).unwrap();
        let channel = Channel::new(&target, Arc::new(composite_creds), Default::default());

        let trailers = perform_unary_echo_failure(&channel).await;
        assert_eq!(trailers.status().code(), StatusCode::Internal);
        assert!(trailers.status().message().contains("test message"));
    }

    shutdown_notify.notify_one();
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

    tx.send(
        &req,
        SendOptions {
            final_msg: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mut resp = WrappedEchoResponse(EchoResponse::default());

    let ClientResponseStreamItem::Headers(headers) = rx.next(&mut resp).await else {
        panic!("Expected Headers first");
    };

    let ClientResponseStreamItem::Message(()) = rx.next(&mut resp).await else {
        panic!("Expected Message after Headers");
    };
    let echo_resp = std::mem::take(&mut resp.0);

    let ClientResponseStreamItem::Trailers(trailers) = rx.next(&mut resp).await else {
        panic!("Expected Trailers, got StreamClosed or other item");
    };

    (headers, echo_resp, trailers)
}

async fn perform_unary_echo_failure(channel: &Channel) -> Trailers {
    let (_tx, mut rx) = channel
        .invoke(
            RequestHeaders::new().with_method_name("/grpc.examples.echo.Echo/UnaryEcho"),
            CallOptions::default(),
        )
        .await;

    let mut resp = WrappedEchoResponse(EchoResponse::default());
    let ClientResponseStreamItem::Trailers(t) = rx.next(&mut resp).await else {
        panic!("Expected Trailers due to failure");
    };
    t
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
pub(crate) struct EchoService {}

#[async_trait]
impl Echo for EchoService {
    async fn unary_echo(
        &self,
        request: tonic::Request<EchoRequest>,
    ) -> std::result::Result<tonic::Response<EchoResponse>, tonic::Status> {
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

    type ServerStreamingEchoStream = ReceiverStream<Result<EchoResponse, tonic::Status>>;

    async fn server_streaming_echo(
        &self,
        _: tonic::Request<EchoRequest>,
    ) -> std::result::Result<tonic::Response<Self::ServerStreamingEchoStream>, tonic::Status> {
        unimplemented!()
    }

    async fn client_streaming_echo(
        &self,
        _: tonic::Request<tonic::Streaming<EchoRequest>>,
    ) -> std::result::Result<tonic::Response<EchoResponse>, tonic::Status> {
        unimplemented!()
    }
    type BidirectionalStreamingEchoStream =
        Pin<Box<dyn Stream<Item = Result<EchoResponse, tonic::Status>> + Send + 'static>>;

    async fn bidirectional_streaming_echo(
        &self,
        request: tonic::Request<tonic::Streaming<EchoRequest>>,
    ) -> std::result::Result<tonic::Response<Self::BidirectionalStreamingEchoStream>, tonic::Status>
    {
        let metadata = request.metadata().clone();
        if let Some(val) = metadata.get("x-test-metadata")
            && val == "test-value"
        {
            println!("Server received expected metadata");
        }
        let mut inbound = request.into_inner();

        // Map each request to a corresponding EchoResponse
        let outbound = async_stream::try_stream! {
            while let Some(req) = inbound.next().await {
                let req = req?; // Return Err(Status) if stream item is error
                let reply = EchoResponse {
                    message: req.message.clone(),
                };
                yield reply;
            }
            println!("Server closing stream");
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::BidirectionalStreamingEchoStream
        ))
    }
}

/// Calculates the relative path from a `base` directory to a `target` path.
/// Both paths should be absolute.
fn get_relative_path(target: &Path, base: &Path) -> Option<PathBuf> {
    let mut target_components = target.components();
    let mut base_components = base.components();

    // Find the common prefix between the two paths.
    let mut common_components = 0;
    loop {
        match (
            target_components.clone().next(),
            base_components.clone().next(),
        ) {
            (Some(t), Some(b)) if t == b => {
                target_components.next();
                base_components.next();
                common_components += 1;
            }
            _ => break,
        }
    }

    // If they share absolutely nothing (e.g., C:\ vs D:\ on Windows), we can't
    // make it relative.
    if common_components == 0 {
        return None;
    }

    let mut relative_path = PathBuf::new();

    // For every component left in the base path, we need to go up one directory
    // ("..").
    for _ in base_components {
        relative_path.push(Component::ParentDir);
    }

    // Append the remaining components of the target path.
    for component in target_components {
        relative_path.push(component);
    }

    Some(relative_path)
}
