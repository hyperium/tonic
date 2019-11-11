pub mod pb {
    tonic::include_proto!("/grpc.examples.echo");
}

use pb::{client::EchoClient, EchoRequest};
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pem = tokio::fs::read("tonic-examples/data/tls/ca.pem").await?;
    let ca = Certificate::from_pem(pem);

    let tls = ClientTlsConfig::with_rustls()
        .ca_certificate(ca)
        .domain_name("example.com");

    let channel = Channel::from_static("http://[::1]:50051")
        .tls_config(tls)
        .connect()
        .await?;

    let mut client = EchoClient::new(channel);
    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = client.unary_echo(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
