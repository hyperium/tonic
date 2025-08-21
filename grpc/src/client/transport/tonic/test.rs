use crate::client::name_resolution::{self, dns, TCP_IP_NETWORK_TYPE};
use crate::client::transport::registry::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::{Channel, ChannelOptions};
use crate::echo_pb::echo_server::{Echo, EchoServer};
use crate::echo_pb::{EchoRequest, EchoResponse};
use crate::service::Request as GrpcRequest;
use crate::service::{Message, MessageAllocator};
use crate::{client::transport::TransportOptions, rt::tokio::TokioRuntime};
use bytes::{Bytes, BytesMut};
use std::any::Any;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::timeout;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};
use tonic::async_trait;
use tonic::{transport::Server, Request, Response, Status};
use tonic_prost::prost::Message as ProstMessage;

const DEFAULT_TEST_DURATION: Duration = Duration::from_secs(10);
const DEFAULT_TEST_SHORT_DURATION: Duration = Duration::from_millis(10);

impl<T> Message for T
where
    T: ProstMessage + Send + Sync + Any + Debug + Default,
{
    fn encode(&self, buf: &mut BytesMut) -> Result<(), String> {
        T::encode(&self, buf).map_err(|err| err.to_string())
    }

    fn decode(&mut self, buf: &Bytes) -> Result<(), String> {
        T::merge(self, buf.as_ref()).map_err(|err| err.to_string())?;
        Ok(())
    }

    fn encoded_message_size_hint(&self) -> Option<usize> {
        Some(T::encoded_len(&self))
    }
}

#[derive(Default)]
struct ProstMessageAllocator<T> {
    _pd: PhantomData<T>,
}

impl<T> MessageAllocator for ProstMessageAllocator<T>
where
    T: ProstMessage + Send + Sync + Any + Debug + Default,
{
    fn allocate(&self) -> Box<dyn Message> {
        Box::new(T::default())
    }
}

// Tests the tonic transport by creating a bi-di stream with a tonic server.
#[tokio::test]
pub async fn grpc_channel_rpc() {
    super::reg();
    dns::reg();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap(); // get the assigned address
    println!("EchoServer listening on: {addr}");
    let server_handle = tokio::spawn(async move {
        let echo_server = EchoService {};
        let svc = EchoServer::new(echo_server);
        let _ = Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await;
    });
    let chan_opts = ChannelOptions::default();
    let chan = Channel::new(&format!("dns:///{addr}"), None, chan_opts);

    let (tx, rx) = mpsc::channel::<Box<dyn Message>>(1);

    // Convert the mpsc receiver into a Stream
    let outbound: GrpcRequest =
        Request::new(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));

    let mut inbound = chan
        .call(
            "/grpc.examples.echo.Echo/BidirectionalStreamingEcho".to_string(),
            outbound,
            Box::new(ProstMessageAllocator::<EchoResponse>::default()),
        )
        .await
        .into_inner();

    // Spawn a sender task
    let client_handle = tokio::spawn(async move {
        for i in 0..5 {
            let message = format!("message {i}");
            let request = EchoRequest {
                message: message.clone(),
            };

            println!("Sent request: {request:?}");
            assert!(tx.send(Box::new(request)).await.is_ok(), "Receiver dropped");

            // Wait for the reply
            let resp = inbound
                .next()
                .await
                .expect("server unexpectedly closed the stream!")
                .expect("server returned error");

            let echo_response = (resp as Box<dyn Any>).downcast::<EchoResponse>().unwrap();
            println!("Got response: {echo_response:?}");
            assert_eq!(echo_response.message, message);
        }
    });

    client_handle.await.unwrap();
}

// Tests the tonic transport by creating a bi-di stream with a tonic server.
#[tokio::test]
pub async fn tonic_transport_rpc() {
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
    let mut connected_transport = builder
        .connect(addr.to_string(), Arc::new(TokioRuntime {}), &config)
        .await
        .unwrap();
    let conn = connected_transport.service;

    let (tx, rx) = mpsc::channel::<Box<dyn Message>>(1);

    // Convert the mpsc receiver into a Stream
    let outbound: GrpcRequest =
        Request::new(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));

    let mut inbound = conn
        .call(
            "/grpc.examples.echo.Echo/BidirectionalStreamingEcho".to_string(),
            outbound,
            Box::new(ProstMessageAllocator::<EchoResponse>::default()),
        )
        .await
        .into_inner();

    // Spawn a sender task
    let client_handle = tokio::spawn(async move {
        for i in 0..5 {
            let message = format!("message {i}");
            let request = EchoRequest {
                message: message.clone(),
            };

            println!("Sent request: {request:?}");
            assert!(tx.send(Box::new(request)).await.is_ok(), "Receiver dropped");

            // Wait for the reply
            let resp = inbound
                .next()
                .await
                .expect("server unexpectedly closed the stream!")
                .expect("server returned error");

            let echo_response = (resp as Box<dyn Any>).downcast::<EchoResponse>().unwrap();
            println!("Got response: {echo_response:?}");
            assert_eq!(echo_response.message, message);
        }
    });

    client_handle.await.unwrap();
    // The connection should break only after the server is stopped.
    assert_eq!(
        connected_transport.disconnection_listener.try_recv(),
        Err(oneshot::error::TryRecvError::Empty),
    );
    shutdown_notify.notify_waiters();
    let res = timeout(
        DEFAULT_TEST_DURATION,
        connected_transport.disconnection_listener,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res, Ok(()));
    server_handle.await.unwrap();
}

#[derive(Debug)]
pub struct EchoService {}

#[async_trait]
impl Echo for EchoService {
    async fn unary_echo(
        &self,
        _: tonic::Request<EchoRequest>,
    ) -> std::result::Result<tonic::Response<EchoResponse>, tonic::Status> {
        unimplemented!()
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
