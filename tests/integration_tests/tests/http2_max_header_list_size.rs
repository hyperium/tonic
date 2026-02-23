use std::time::Duration;

use integration_tests::pb::{test_client, test_server, Input, Output};
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};

/// This test checks that the max header list size is respected, and that
/// it allows for error messages up to that size.
#[tokio::test]
async fn test_http_max_header_list_size_and_long_errors() {
    struct Svc;

    // The default value is 16k.
    const N: usize = 20_000;

    fn long_message() -> String {
        "a".repeat(N)
    }

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Err(Status::internal(long_message()))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel::<()>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());

    let jh = tokio::spawn(async move {
        let (nodelay, keepalive) = (Some(true), Some(Duration::from_secs(1)));
        let listener = tonic::transport::server::TcpIncoming::from(listener)
            .with_nodelay(nodelay)
            .with_keepalive(keepalive);
        Server::builder()
            .http2_max_pending_accept_reset_streams(Some(0))
            .add_service(svc)
            .serve_with_incoming_shutdown(listener, async { drop(rx.await) })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let channel = Endpoint::from_shared(addr)
        .unwrap()
        // Increase the max header list size to 2x the default value. If this is
        // not set, this test will hang. See
        // <https://github.com/hyperium/tonic/issues/1834>.
        .http2_max_header_list_size(u32::try_from(N * 2).unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = test_client::TestClient::new(channel);

    let err = client.unary_call(Request::new(Input {})).await.unwrap_err();

    assert_eq!(err.message(), long_message());

    tx.send(()).unwrap();

    jh.await.unwrap();
}
