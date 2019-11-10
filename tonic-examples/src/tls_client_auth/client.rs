pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use pb::{client::EchoClient, EchoRequest};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_root_ca_cert = tokio::fs::read("tonic-examples/data/tls/ca.pem").await?;
    let server_root_ca_cert = Certificate::from_pem(server_root_ca_cert);
    let client_cert = tokio::fs::read("tonic-examples/data/tls/client1.pem").await?;
    let client_key = tokio::fs::read("tonic-examples/data/tls/client1.key").await?;
    let client_identity = Identity::from_pem(client_cert, client_key);

    let tls = ClientTlsConfig::with_rustls()
        .domain_name("localhost")
        .ca_certificate(server_root_ca_cert)
        .identity(client_identity);

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
