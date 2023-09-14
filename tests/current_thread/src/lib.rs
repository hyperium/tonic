#![allow(unused_imports)]

use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};

tonic::include_proto!("test");

#[derive(Debug, Default)]
struct Svc;

#[tonic::async_trait(?Send)]
impl test_server::Test for Svc {
    async fn test_request(&self, req: Request<SomeData>) -> Result<Response<SomeData>, Status> {
        Ok(Response::new(req.into_inner()))
    }
}

#[tokio::test]
async fn test() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let svc = Svc::default();

    let server = tonic::transport::Server::builder()
        .local_executor()
        .add_service(test_server::TestServer::new(svc))
        .serve(addr);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            tokio::task::spawn_local(server);
        })
        .await;

    Ok(())
}
