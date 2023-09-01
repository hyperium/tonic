use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    Code, Request, Response, Status,
};

struct Svc(Arc<Mutex<Option<oneshot::Sender<()>>>>);

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
        let mut l = self.0.lock().unwrap();
        l.take().unwrap().send(()).unwrap();

        Ok(Response::new(Output {}))
    }
}

#[tokio::test]
async fn connect_returns_err() {
    let res = TestClient::connect("http://thisdoesntexist").await;

    assert!(res.is_err());
}

#[tokio::test]
async fn connect_returns_err_via_call_after_connected() {
    let (tx, rx) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let svc = test_server::TestServer::new(Svc(sender));

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1338".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TestClient::connect("http://127.0.0.1:1338").await.unwrap();

    // First call should pass, then shutdown the server
    client.unary_call(Request::new(Input {})).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let res = client.unary_call(Request::new(Input {})).await;

    let err = res.unwrap_err();
    assert_eq!(err.code(), Code::Unavailable);

    jh.await.unwrap();
}

#[tokio::test]
async fn connect_lazy_reconnects_after_first_failure() {
    let (tx, rx) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let svc = test_server::TestServer::new(Svc(sender));

    let channel = Endpoint::from_static("http://127.0.0.1:1339").connect_lazy();

    let mut client = TestClient::new(channel);

    // First call should fail, the server is not running
    client.unary_call(Request::new(Input {})).await.unwrap_err();

    // Start the server now, second call should succeed
    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1339".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.unary_call(Request::new(Input {})).await.unwrap();

    // The server shut down, third call should fail
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = client.unary_call(Request::new(Input {})).await.unwrap_err();

    assert_eq!(err.code(), Code::Unavailable);

    jh.await.unwrap();
}
