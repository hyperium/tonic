use interop::server::{EchoHeadersSvc, TestService, TestServiceServer};
use tonic::transport::Server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = ([127, 0, 0, 1], 9999).into();
    let test_svc = TestServiceServer::new(TestService::default());
    let with_echo = EchoHeadersSvc::new(test_svc);

    Server::builder()
        .accept_http1(true)
        .add_service(tonic_web::enable(with_echo))
        .serve(addr)
        .await?;

    Ok(())
}
