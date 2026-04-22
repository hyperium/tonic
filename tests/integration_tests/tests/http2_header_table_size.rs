use std::time::Duration;

use integration_tests::pb::{test_client, test_server, Input, Output};
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};

#[tokio::test]
async fn http2_header_table_size_zero() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());

    let jh = tokio::spawn(async move {
        let listener =
            tonic::transport::server::TcpIncoming::from(listener).with_nodelay(Some(true));
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(listener, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(addr)
        .unwrap()
        .http2_header_table_size(0)
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    client.unary_call(Request::new(Input {})).await.unwrap();

    tx.send(()).unwrap();
    jh.await.unwrap();
}
