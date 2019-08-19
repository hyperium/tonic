use crate::{pb::*, test_assert, TestAssertion};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tonic::Request;
use tower_h2::{add_origin::AddOrigin, Connection};

pub type Client = TestServiceClient<AddOrigin<Connection<tonic::BoxBody>>>;

tonic::client!(service = "grpc.testing.TestService", proto = "crate::pb");

pub async fn create(addr: SocketAddr) -> Result<Client, Box<dyn std::error::Error>> {
    let io = TcpStream::connect(&addr).await?;

    let origin = http::Uri::from_shared(format!("http://{}", addr).into()).unwrap();

    let svc = Connection::handshake(io).await?;
    let svc = AddOrigin::new(svc, origin);

    Ok(TestServiceClient::new(svc))
}

pub async fn unary_call(client: &mut Client, assertions: &mut Vec<TestAssertion>) {
    let result = client.empty_call(Request::new(Empty {})).await;

    assertions.push(test_assert!(
        "call must be successful",
        result.is_ok(),
        format!("result={:?}", result)
    ));

    if let Ok(response) = result {
        let body = response.into_inner();
        assertions.push(test_assert!(
            "body must not be null",
            body == Empty {},
            format!("body={:?}", body)
        ));
    }
}
