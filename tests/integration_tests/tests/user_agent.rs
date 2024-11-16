use integration_tests::pb::{test_client, test_server, Input, Output};
use std::time::Duration;
use tokio::{net::TcpListener, sync::oneshot};
use tonic::{
    transport::{server::TcpIncoming, Endpoint, Server},
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

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
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
