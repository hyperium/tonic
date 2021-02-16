use futures::stream;
use futures_util::FutureExt;
use tokio::sync::oneshot;
use tonic::transport::Server;
use tonic::Request;
use tonic_reflection::server::Builder;

use pb::server_reflection_client::ServerReflectionClient;
use pb::server_reflection_request::MessageRequest;
use pb::server_reflection_response::MessageResponse;
use pb::ServerReflectionRequest;
use pb::ServiceResponse;
use std::net::SocketAddr;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::StreamExt;

mod pb {
    #![allow(unreachable_pub)]
    use prost::Message;

    tonic::include_proto!("grpc.reflection.v1alpha");

    pub(crate) const REFLECTION_SERVICE_DESCRIPTOR: &'static [u8] =
        tonic::include_file_descriptor_set!("reflection_v1alpha1");

    pub(crate) fn get_encoded_reflection_service_fd() -> Vec<u8> {
        let mut expected = Vec::new();
        &prost_types::FileDescriptorSet::decode(REFLECTION_SERVICE_DESCRIPTOR)
            .expect("decode reflection service file descriptor set")
            .file[0]
            .encode(&mut expected)
            .expect("encode reflection service file descriptor");
        expected
    }
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
            pb::get_encoded_reflection_service_fd()
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
            pb::get_encoded_reflection_service_fd()
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
    let local_addr = listener.local_addr().expect("local address");
    let local_addr = format!("http://{}", local_addr.to_string());
    let jh = tokio::spawn(async move {
        let service = Builder::configure()
            .register_encoded_file_descriptor_set(pb::REFLECTION_SERVICE_DESCRIPTOR)
            .build()
            .unwrap();

        Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown_rx.map(drop))
            .await
            .unwrap();
    });

    // Give the test server a few ms to become available
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Construct client and send request, extract response
    let mut client = ServerReflectionClient::connect(local_addr)
        .await
        .expect("connect");

    let request = Request::new(stream::iter(vec![request]));
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
