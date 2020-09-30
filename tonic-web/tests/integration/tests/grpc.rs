use std::fmt::Debug;

use integration::pb::test_client::TestClient;
use integration::pb::test_server::{Test, TestServer};
use integration::pb::{Input, Output};
use tokio::time::Duration;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tonic_web::GrpcWeb;

struct Svc;

async fn sleep(millis: u64) {
    tokio::time::delay_for(Duration::from_millis(millis)).await
}

#[tonic::async_trait]
impl Test for Svc {
    async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
        Ok(Response::new(Output {}))
    }
}

fn assert_equal_response<T: PartialEq + Debug>(a: Response<T>, b: Response<T>) {
    assert_eq!(format!("{:?}", a.metadata()), format!("{:?}", b.metadata()));
    assert_eq!(a.into_inner(), b.into_inner());
}

#[tokio::test]
async fn smoke_integration() {
    let grpc_addr = ([127, 0, 0, 1], 1234).into();
    let grpc_web_addr = ([127, 0, 0, 1], 1235).into();

    let grpc = TestServer::new(Svc);
    let grpc_web = GrpcWeb::new(grpc.clone());

    let h1 = tokio::spawn(async move {
        Server::builder()
            .add_service(grpc)
            .serve(grpc_addr)
            .await
            .unwrap();
    });

    let h2 = tokio::spawn(async move {
        Server::builder()
            .add_service(grpc_web)
            .serve(grpc_web_addr)
            .await
            .unwrap();
    });

    sleep(10).await;

    let (mut grpc_client, mut grpc_web_client) = tokio::try_join!(
        TestClient::connect(format!("http://{}", grpc_addr)),
        TestClient::connect(format!("http://{}", grpc_web_addr))
    )
    .unwrap();

    let (res1, res2) = tokio::try_join!(
        grpc_client.unary_call(Input {}),
        grpc_web_client.unary_call(Input {})
    )
    .unwrap();

    assert_equal_response(res1, res2);

    drop(h1);
    drop(h2);
}
