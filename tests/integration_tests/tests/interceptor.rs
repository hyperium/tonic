use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use std::time::Duration;
use tokio::sync::oneshot;
use tonic::{
    transport::{Endpoint, Server},
    GrpcMethod, Request, Response, Status,
};

#[tokio::test]
async fn interceptor_retrieves_grpc_method() {
    use test_server::Test;

    struct Svc;

    #[tonic::async_trait]
    impl Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Ok(Response::new(Output {}))
        }
    }

    let svc = test_server::TestServer::new(Svc);

    let (tx, rx) = oneshot::channel();
    // Start the server now, second call should succeed
    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1340".parse().unwrap(), async { drop(rx.await) })
            .await
            .unwrap();
    });

    let channel = Endpoint::from_static("http://127.0.0.1:1340").connect_lazy();

    fn client_intercept(req: Request<()>) -> Result<Request<()>, Status> {
        println!("Intercepting client request: {:?}", req);

        let gm = req.extensions().get::<GrpcMethod>().unwrap();
        assert_eq!(gm.service(), "test.Test");
        assert_eq!(gm.method(), "UnaryCall");

        Ok(req)
    }
    let mut client = TestClient::with_interceptor(channel, client_intercept);

    tokio::time::sleep(Duration::from_millis(100)).await;
    client.unary_call(Request::new(Input {})).await.unwrap();

    tx.send(()).unwrap();
    jh.await.unwrap();
}
