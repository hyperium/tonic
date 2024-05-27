use bytes::Bytes;
use http::Uri;
use hyper_util::rt::TokioIo;
use integration_tests::mock::MockStream;
use integration_tests::pb::{
    test_client, test_server, test_stream_client, test_stream_server, Input, InputStream, Output,
    OutputStream,
};
use std::error::Error;
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::transport::Endpoint;
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
            .serve_with_shutdown("127.0.0.1:1337".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

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
            .serve_with_shutdown("127.0.0.1:1338".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

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

type Stream<T> = std::pin::Pin<
    Box<dyn tokio_stream::Stream<Item = std::result::Result<T, Status>> + Send + 'static>,
>;

#[tokio::test]
async fn status_from_server_stream() {
    integration_tests::trace_init();

    struct Svc;

    #[tonic::async_trait]
    impl test_stream_server::TestStream for Svc {
        type StreamCallStream = Stream<OutputStream>;

        async fn stream_call(
            &self,
            _: Request<InputStream>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let s = tokio_stream::iter(vec![
                Err::<OutputStream, _>(Status::unavailable("foo")),
                Err::<OutputStream, _>(Status::unavailable("bar")),
            ]);
            Ok(Response::new(Box::pin(s) as Self::StreamCallStream))
        }
    }

    let svc = test_stream_server::TestStreamServer::new(Svc);

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve("127.0.0.1:1339".parse().unwrap())
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = test_stream_client::TestStreamClient::connect("http://127.0.0.1:1339")
        .await
        .unwrap();

    let mut stream = client
        .stream_call(InputStream {})
        .await
        .unwrap()
        .into_inner();

    assert_eq!(stream.message().await.unwrap_err().message(), "foo");
    assert_eq!(stream.message().await.unwrap(), None);
}

#[tokio::test]
async fn status_from_server_stream_with_source() {
    integration_tests::trace_init();

    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector_lazy(tower::service_fn(move |_: Uri| async move {
            Err::<TokioIo<MockStream>, _>(std::io::Error::new(std::io::ErrorKind::Other, "WTF"))
        }));

    let mut client = test_stream_client::TestStreamClient::new(channel);

    let error = client.stream_call(InputStream {}).await.unwrap_err();

    let source = error.source().unwrap();
    source.downcast_ref::<tonic::transport::Error>().unwrap();
}
