pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/grpc.examples.echo.rs"));
}

use pb::{client::EchoClient, EchoRequest};
use tonic::transport::{Certificate, Channel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pem = tokio::fs::read("tonic-examples/data/tls/ca.pem").await?;
    let ca = Certificate::from_pem(pem);

    let channel = Channel::from_static("http://[::1]:50051")
        .rustls_tls(ca, Some("example.com".into()))
        .channel();

    let mut client = EchoClient::new(channel);

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = client.unary_echo(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
