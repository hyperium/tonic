use futures_util::FutureExt;
use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::Server,
    Request, Response, Status,
};

#[tokio::test]
async fn cancelation_on_timeout() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _req: Request<Input>) -> Result<Response<Output>, Status> {
            // Wait for a time longer than the timeout
            tokio::time::delay_for(Duration::from_millis(1_000)).await;
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1322".parse().unwrap(), rx.map(drop))
            .await
            .unwrap();
    });

    tokio::time::delay_for(Duration::from_millis(100)).await;

    let mut client = test_client::TestClient::connect("http://127.0.0.1:1322")
        .await
        .unwrap();

    let mut req = Request::new(Input {});
    req.metadata_mut().insert("grpc-timeout", "500m".parse().unwrap());

    let err = client.unary_call(req).await.unwrap_err();
    assert!(err.message().contains("Timeout expired"));
    // TODO: Need to return the correct type of code
    // assert_eq!(err.code(), Code::Cancelled);

    tx.send(()).unwrap();

    jh.await.unwrap();
}