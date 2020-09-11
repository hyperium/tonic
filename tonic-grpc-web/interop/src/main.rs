use tonic::transport::Server;
use tonic_grpc_web::GrpcWeb;

use crate::echo_service::EchoHeadersSvc;
use crate::pb::test_service_server::TestServiceServer;
use crate::pb::unimplemented_service_server::UnimplementedServiceServer;
use crate::test_service::Test;
use crate::unimplemented_service::Unimplemented;
use std::net::SocketAddr;

mod pb {
    tonic::include_proto!("grpc.testing");
}

mod echo_service;
mod test_service;
mod unimplemented_service;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = ([127, 0, 0, 1], 9999).into();

    let test_service = TestServiceServer::new(Test);
    let echo_service = EchoHeadersSvc::new(test_service);

    single_service(echo_service, addr).await
}

#[allow(unused)]
async fn single_service(
    echo: EchoHeadersSvc<TestServiceServer<Test>>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let with_grpc_web = GrpcWeb::new(echo);

    Server::builder()
        .add_service(with_grpc_web)
        .serve(addr)
        .await
        .map_err(Into::into)
}

#[allow(unused)]
async fn multiplex(
    echo: EchoHeadersSvc<TestServiceServer<Test>>,
    addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let unimplemented = GrpcWeb::new(UnimplementedServiceServer::new(Unimplemented));
    let echo = GrpcWeb::new(echo);

    Server::builder()
        .add_service(unimplemented)
        .add_service(echo)
        .serve(addr)
        .await
        .map_err(Into::into)
}
