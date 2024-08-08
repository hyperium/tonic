use std::net::SocketAddr;
use tokio::sync::oneshot;
use tokio_stream::{wrappers::TcpListenerStream, StreamExt};
use tonic::{transport::Server, Request};

use tonic_reflection::pb::{v1, v1alpha};
use tonic_reflection::server::v1alpha::Builder as V1AlphaBuilder;
use tonic_reflection::server::Builder;

#[tokio::test]
async fn test_v1() {
    let response = make_v1_request(v1::ServerReflectionRequest {
        host: "".to_string(),
        message_request: Some(v1::server_reflection_request::MessageRequest::ListServices(
            String::new(),
        )),
    })
    .await;

    if let v1::server_reflection_response::MessageResponse::ListServicesResponse(services) =
        response
    {
        assert_eq!(
            services.service,
            vec![v1::ServiceResponse {
                name: String::from("grpc.reflection.v1.ServerReflection")
            }]
        );
    } else {
        panic!("Expected a ListServicesResponse variant");
    }
}

#[tokio::test]
async fn test_v1alpha() {
    let response = make_v1alpha_request(v1alpha::ServerReflectionRequest {
        host: "".to_string(),
        message_request: Some(
            v1alpha::server_reflection_request::MessageRequest::ListServices(String::new()),
        ),
    })
    .await;

    if let v1alpha::server_reflection_response::MessageResponse::ListServicesResponse(services) =
        response
    {
        assert_eq!(
            services.service,
            vec![v1alpha::ServiceResponse {
                name: String::from("grpc.reflection.v1alpha.ServerReflection")
            }]
        );
    } else {
        panic!("Expected a ListServicesResponse variant");
    }
}

async fn make_v1_request(
    request: v1::ServerReflectionRequest,
) -> v1::server_reflection_response::MessageResponse {
    // Run a test server
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let addr: SocketAddr = "127.0.0.1:0".parse().expect("SocketAddr parse");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    let local_addr = format!("http://{}", listener.local_addr().expect("local address"));
    let jh = tokio::spawn(async move {
        let service = Builder::configure().build().unwrap();

        Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                drop(shutdown_rx.await)
            })
            .await
            .unwrap();
    });

    // Give the test server a few ms to become available
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Construct client and send request, extract response
    let conn = tonic::transport::Endpoint::new(local_addr)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = v1::server_reflection_client::ServerReflectionClient::new(conn);

    let request = Request::new(tokio_stream::once(request));
    let mut inbound = client
        .server_reflection_info(request)
        .await
        .expect("request")
        .into_inner();

    let response = inbound
        .next()
        .await
        .expect("steamed response")
        .expect("successful response")
        .message_response
        .expect("some MessageResponse");

    // We only expect one response per request
    assert!(inbound.next().await.is_none());

    // Shut down test server
    shutdown_tx.send(()).expect("send shutdown");
    jh.await.expect("server shutdown");

    response
}

async fn make_v1alpha_request(
    request: v1alpha::ServerReflectionRequest,
) -> v1alpha::server_reflection_response::MessageResponse {
    // Run a test server
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let addr: SocketAddr = "127.0.0.1:0".parse().expect("SocketAddr parse");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    let local_addr = format!("http://{}", listener.local_addr().expect("local address"));
    let jh = tokio::spawn(async move {
        let service = V1AlphaBuilder::configure().build().unwrap();

        Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                drop(shutdown_rx.await)
            })
            .await
            .unwrap();
    });

    // Give the test server a few ms to become available
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Construct client and send request, extract response
    let conn = tonic::transport::Endpoint::new(local_addr)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = v1alpha::server_reflection_client::ServerReflectionClient::new(conn);

    let request = Request::new(tokio_stream::once(request));
    let mut inbound = client
        .server_reflection_info(request)
        .await
        .expect("request")
        .into_inner();

    let response = inbound
        .next()
        .await
        .expect("steamed response")
        .expect("successful response")
        .message_response
        .expect("some MessageResponse");

    // We only expect one response per request
    assert!(inbound.next().await.is_none());

    // Shut down test server
    shutdown_tx.send(()).expect("send shutdown");
    jh.await.expect("server shutdown");

    response
}
