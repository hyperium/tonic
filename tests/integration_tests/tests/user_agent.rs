use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};

#[tokio::test]
async fn writes_user_agent_header() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            match req.metadata().get("user-agent") {
                Some(_) => Ok(Response::new(Output {})),
                None => Err(Status::internal("user-agent header is missing")),
            }
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1322".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_static("http://127.0.0.1:1322")
        .user_agent("my-client")
        .expect("valid user agent")
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    match client.unary_call(Input {}).await {
        Ok(_) => {}
        Err(status) => panic!("{}", status.message()),
    }

    tx.send(()).unwrap();

    jh.await.unwrap();
}
