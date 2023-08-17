pub mod pb {
    tonic::include_proto!("/grpc.examples.unaryecho");
}

use pb::{echo_client::EchoClient, EchoRequest};
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let pem = std::fs::read_to_string(data_dir.join("tls/ca.pem"))?;
    let ca = Certificate::from_pem(pem);

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name("example.com");

    let channel = Channel::from_static("https://[::1]:50051")
        .tls_config(tls)?
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
