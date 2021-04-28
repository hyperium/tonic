use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::net::TcpListener;
use tonic::{transport::Server, Code, Request, Response, Status};

#[tokio::test]
async fn cancelation_on_timeout() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            // Wait for a time longer than the timeout
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let mut client = test_client::TestClient::connect(format!("http://{}", addr))
        .await
        .unwrap();

    let mut req = Request::new(Input {});
    req.metadata_mut()
        .insert("grpc-timeout", "500m".parse().unwrap());

    let res = client.unary_call(req).await;

    let err = res.unwrap_err();
    assert!(err.message().contains("Timeout expired"));
    assert_eq!(err.code(), Code::Cancelled);
}

#[tokio::test]
async fn picks_the_shortest_timeout() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            // Wait for a time longer than the timeout
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .timeout(Duration::from_millis(100))
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let mut client = test_client::TestClient::connect(format!("http://{}", addr))
        .await
        .unwrap();

    let mut req = Request::new(Input {});
    req.metadata_mut()
        // 10 hours
        .insert("grpc-timeout", "10H".parse().unwrap());

    // TODO(david): for some reason this fails with "h2 protocol error: protocol error: unexpected
    // internal error encountered". Seems to be happening on `master` as well. Bug?
    let res = client.unary_call(req).await;
    dbg!(&res);
    let err = res.unwrap_err();
    assert!(err.message().contains("Timeout expired"));
}
