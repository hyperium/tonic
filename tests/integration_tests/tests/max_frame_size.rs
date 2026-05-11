use tokio::net::TcpListener;
use tokio::sync::oneshot;

use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use tonic::transport::{server::TcpIncoming, Channel, Server};
use tonic::{Request, Response, Status};

struct Svc;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
        Ok(Response::new(Output {}))
    }
}

#[tokio::test]
async fn max_frame_size_on_client_endpoint() {
    let svc = test_server::TestServer::new(Svc {});
    let (tx, rx) = oneshot::channel::<()>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    let jh = tokio::spawn(async move {
        Server::builder()
            .max_frame_size(1024 * 1024u32) // 1 MB
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Set client-side max_frame_size to match server
    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .max_frame_size(1024 * 1024u32)
        .connect()
        .await
        .unwrap();
    let mut client = TestClient::new(channel);

    let res = client.unary_call(Request::new(Input {})).await;

    assert!(res.is_ok());

    tx.send(()).unwrap();
    jh.await.unwrap();
}
