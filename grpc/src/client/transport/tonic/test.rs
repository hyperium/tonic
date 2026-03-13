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

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Buf;
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Response;
use tonic::Status;
use tonic::async_trait;
use tonic::transport::Server;
use tonic_prost::prost::Message as ProstMessage;

use crate::client::CallOptions;
use crate::client::Channel;
use crate::client::Invoke as _;
use crate::client::RecvStream as _;
use crate::client::SendOptions;
use crate::client::SendStream as _;
use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::client::transport::TransportOptions;
use crate::client::transport::registry::GLOBAL_TRANSPORT_REGISTRY;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::SendMessage;
use crate::credentials::InsecureChannelCredentials;
use crate::echo_pb::EchoRequest;
use crate::echo_pb::EchoResponse;
use crate::echo_pb::echo_server::Echo;
use crate::echo_pb::echo_server::EchoServer;
use crate::rt::GrpcRuntime;
use crate::rt::tokio::TokioRuntime;

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
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    let builder = GLOBAL_TRANSPORT_REGISTRY
        .get_transport(TCP_IP_NETWORK_TYPE)
        .unwrap();
    let config = Arc::new(TransportOptions::default());
    let (conn, mut disconnection_listener) = builder
        .dyn_connect(
            addr.to_string(),
            GrpcRuntime::new(TokioRuntime::default()),
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
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                shutdown_notify_copy.notified(),
            )
            .await;
    });

    // Create the channel.
    let target = format!("dns:///{}", addr);
    let channel = Channel::new(
        &target,
        InsecureChannelCredentials::new(),
        Default::default(),
    );

    // Start the call.
    let (mut tx, mut rx) = channel
        .invoke(
            RequestHeaders::new().with_method_name("/grpc.examples.echo.Echo/UnaryEcho"),
            CallOptions::default(),
        )
        .await;

    // Send the request.
    let req = WrappedEchoRequest(EchoRequest {
        message: "hello interop".into(),
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

    // Response should be Headers, Message ("hello interop"), Trailers (OK).
    let mut resp = WrappedEchoResponse(EchoResponse::default());

    let ClientResponseStreamItem::Headers(_) = rx.next(&mut resp).await else {
        panic!("Expected Headers first");
    };

    let ClientResponseStreamItem::Message(()) = rx.next(&mut resp).await else {
        panic!("Expected Message after Headers");
    };
    assert_eq!(resp.0.message, "hello interop");

    let ClientResponseStreamItem::Trailers(t) = rx.next(&mut resp).await else {
        panic!("Expected Trailers, got StreamClosed or other item");
    };

    assert_eq!(
        t.status().code(),
        crate::StatusCode::Ok,
        "RPC failed: {:?}",
        t.status()
    );

    shutdown_notify.notify_one();
    server_handle.await.unwrap();
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
        let message = request.into_inner().message;
        Ok(tonic::Response::new(EchoResponse { message }))
    }

    type ServerStreamingEchoStream = ReceiverStream<Result<EchoResponse, Status>>;

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
        Pin<Box<dyn Stream<Item = Result<EchoResponse, Status>> + Send + 'static>>;

    async fn bidirectional_streaming_echo(
        &self,
        request: tonic::Request<tonic::Streaming<EchoRequest>>,
    ) -> std::result::Result<tonic::Response<Self::BidirectionalStreamingEchoStream>, tonic::Status>
    {
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
