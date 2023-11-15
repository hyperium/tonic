use prost::Message;
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tokio_stream::{wrappers::TcpListenerStream, StreamExt};
use tonic::{transport::Server, Request};
use tonic_reflection::{
    pb::{
        server_reflection_client::ServerReflectionClient,
        server_reflection_request::MessageRequest, server_reflection_response::MessageResponse,
        ServerReflectionRequest, ServiceResponse, FILE_DESCRIPTOR_SET,
    },
    server::Builder,
};

pub(crate) fn get_encoded_reflection_service_fd() -> Vec<u8> {
    let mut expected = Vec::new();
    prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
        .expect("decode reflection service file descriptor set")
        .file[0]
        .encode(&mut expected)
        .expect("encode reflection service file descriptor");
    expected
}

#[tokio::test]
async fn test_list_services() {
    let response = make_test_reflection_request(ServerReflectionRequest {
        host: "".to_string(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    })
    .await;

    if let MessageResponse::ListServicesResponse(services) = response {
        assert_eq!(
            services.service,
            vec![ServiceResponse {
                name: String::from("grpc.reflection.v1alpha.ServerReflection")
            }]
        );
    } else {
        panic!("Expected a ListServicesResponse variant");
    }
}

#[tokio::test]
async fn test_file_by_filename() {
    let response = make_test_reflection_request(ServerReflectionRequest {
        host: "".to_string(),
        message_request: Some(MessageRequest::FileByFilename(String::from(
            "reflection.proto",
        ))),
    })
    .await;

    if let MessageResponse::FileDescriptorResponse(descriptor) = response {
        let file_descriptor_proto = descriptor
            .file_descriptor_proto
            .first()
            .expect("descriptor");
        assert_eq!(
            file_descriptor_proto.as_ref(),
            get_encoded_reflection_service_fd()
        );
    } else {
        panic!("Expected a FileDescriptorResponse variant");
    }
}

#[tokio::test]
async fn test_file_containing_symbol() {
    let response = make_test_reflection_request(ServerReflectionRequest {
        host: "".to_string(),
        message_request: Some(MessageRequest::FileContainingSymbol(String::from(
            "grpc.reflection.v1alpha.ServerReflection",
        ))),
    })
    .await;

    if let MessageResponse::FileDescriptorResponse(descriptor) = response {
        let file_descriptor_proto = descriptor
            .file_descriptor_proto
            .first()
            .expect("descriptor");
        assert_eq!(
            file_descriptor_proto.as_ref(),
            get_encoded_reflection_service_fd()
        );
    } else {
        panic!("Expected a FileDescriptorResponse variant");
    }
}

async fn make_test_reflection_request(request: ServerReflectionRequest) -> MessageResponse {
    // Run a test server
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let addr: SocketAddr = "127.0.0.1:0".parse().expect("SocketAddr parse");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    let local_addr = format!("http://{}", listener.local_addr().expect("local address"));
    let jh = tokio::spawn(async move {
        let service = Builder::configure()
            .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
            .build()
            .unwrap();

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
    let mut client = ServerReflectionClient::new(conn);

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
