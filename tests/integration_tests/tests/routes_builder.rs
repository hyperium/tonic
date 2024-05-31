use std::time::Duration;

use tokio::sync::oneshot;
use tokio_stream::StreamExt;

use integration_tests::pb::{
    test1_client, test1_server, test_client, test_server, Input, Input1, Output, Output1,
};
use tonic::codegen::BoxStream;
use tonic::transport::server::RoutesBuilder;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};

#[tokio::test]
async fn multiple_service_using_routes_builder() {
    struct Svc1;

    #[tonic::async_trait]
    impl test_server::Test for Svc1 {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    struct Svc2;

    #[tonic::async_trait]
    impl test1_server::Test1 for Svc2 {
        async fn unary_call(&self, request: Request<Input1>) -> Result<Response<Output1>, Status> {
            Ok(Response::new(Output1 {
                buf: request.into_inner().buf,
            }))
        }

        type StreamCallStream = BoxStream<Output1>;

        async fn stream_call(
            &self,
            request: Request<Input1>,
        ) -> Result<Response<Self::StreamCallStream>, Status> {
            let output = Output1 {
                buf: request.into_inner().buf,
            };
            let stream = tokio_stream::once(Ok(output));

            Ok(Response::new(Box::pin(stream)))
        }
    }

    let svc1 = test_server::TestServer::new(Svc1);
    let svc2 = test1_server::Test1Server::new(Svc2);

    let (tx, rx) = oneshot::channel::<()>();
    let mut routes_builder = RoutesBuilder::default();
    routes_builder.add_service(svc1).add_service(svc2);

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_routes(routes_builder.routes())
            .serve_with_shutdown("127.0.0.1:1400".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_static("http://127.0.0.1:1400")
        .connect()
        .await
        .unwrap();

    let mut client1 = test_client::TestClient::new(channel.clone());
    let mut client2 = test1_client::Test1Client::new(channel);

    client1.unary_call(Input {}).await.unwrap();

    let resp2 = client2
        .unary_call(Input1 {
            buf: b"hello".to_vec(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(&resp2.buf, b"hello");
    let mut stream_response = client2
        .stream_call(Input1 {
            buf: b"world".to_vec(),
        })
        .await
        .unwrap()
        .into_inner();
    let first = match stream_response.next().await {
        Some(Ok(first)) => first,
        _ => panic!("expected one non-error item in the stream call response"),
    };

    assert_eq!(&first.buf, b"world");
    assert!(stream_response.next().await.is_none());

    tx.send(()).unwrap();

    jh.await.unwrap();
}
