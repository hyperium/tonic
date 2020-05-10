use bytes::Bytes;
use futures_util::FutureExt;
use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::{transport::Server, Code, Request, Response, Status};

#[tokio::test]
async fn status_with_details() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Err(Status::with_details(
                Code::ResourceExhausted,
                "Too many requests",
                Bytes::from_static(&[1]),
            ))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1337".parse().unwrap(), rx.map(drop))
            .await
            .unwrap();
    });

    tokio::time::delay_for(Duration::from_millis(100)).await;

    let mut channel = test_client::TestClient::connect("http://127.0.0.1:1337")
        .await
        .unwrap();

    let err = channel
        .unary_call(Request::new(Input {}))
        .await
        .unwrap_err();

    assert_eq!(err.message(), "Too many requests");
    assert_eq!(err.details(), &[1]);

    tx.send(()).unwrap();

    jh.await.unwrap();
}

#[tokio::test]
async fn status_with_metadata() {
    const MESSAGE: &str = "Internal error, see metadata for details";

    const ASCII_NAME: &str = "x-host-ip";
    const ASCII_VALUE: &str = "127.0.0.1";

    const BINARY_NAME: &str = "x-host-name-bin";
    const BINARY_VALUE: &[u8] = b"localhost";

    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            let mut metadata = MetadataMap::new();
            metadata.insert(ASCII_NAME, ASCII_VALUE.parse().unwrap());
            metadata.insert_bin(BINARY_NAME, MetadataValue::from_bytes(BINARY_VALUE));

            Err(Status::with_metadata(Code::Internal, MESSAGE, metadata))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1338".parse().unwrap(), rx.map(drop))
            .await
            .unwrap();
    });

    tokio::time::delay_for(Duration::from_millis(100)).await;

    let mut channel = test_client::TestClient::connect("http://127.0.0.1:1338")
        .await
        .unwrap();

    let err = channel
        .unary_call(Request::new(Input {}))
        .await
        .unwrap_err();

    assert_eq!(err.code(), Code::Internal);
    assert_eq!(err.message(), MESSAGE);

    let metadata = err.metadata();

    assert_eq!(
        metadata.get(ASCII_NAME).unwrap().to_str().unwrap(),
        ASCII_VALUE
    );

    assert_eq!(
        metadata.get_bin(BINARY_NAME).unwrap().to_bytes().unwrap(),
        BINARY_VALUE
    );

    tx.send(()).unwrap();

    jh.await.unwrap();
}
