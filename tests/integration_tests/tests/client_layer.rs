use http::{header::HeaderName, HeaderValue};
use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    Request, Response, Status,
};
use tower::ServiceBuilder;
use tower_http::{set_header::SetRequestHeaderLayer, trace::TraceLayer};

#[tokio::test]
async fn connect_supports_standard_tower_layers() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            match req.metadata().get("x-test") {
                Some(_) => Ok(Response::new(Output {})),
                None => Err(Status::internal("user-agent header is missing")),
            }
        }
    }

    let (tx, rx) = oneshot::channel();
    let svc = test_server::TestServer::new(Svc);

    // Start the server now, second call should succeed
    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1340".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    let channel = Endpoint::from_static("http://127.0.0.1:1340").connect_lazy();

    // prior to https://github.com/hyperium/tonic/pull/974
    // this would not compile. (specifically the `TraceLayer`)
    let mut client = TestClient::new(
        ServiceBuilder::new()
            .layer(SetRequestHeaderLayer::overriding(
                HeaderName::from_static("x-test"),
                HeaderValue::from_static("test-header"),
            ))
            .layer(TraceLayer::new_for_grpc())
            .service(channel),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.unary_call(Request::new(Input {})).await.unwrap();

    tx.send(()).unwrap();
    jh.await.unwrap();
}
