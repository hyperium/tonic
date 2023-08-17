pub mod pb {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use pb::{echo_client::EchoClient, EchoRequest};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let server_root_ca_cert = std::fs::read_to_string(data_dir.join("tls/ca.pem"))?;
    let server_root_ca_cert = Certificate::from_pem(server_root_ca_cert);
    let client_cert = std::fs::read_to_string(data_dir.join("tls/client1.pem"))?;
    let client_key = std::fs::read_to_string(data_dir.join("tls/client1.key"))?;
    let client_identity = Identity::from_pem(client_cert, client_key);

    let tls = ClientTlsConfig::new()
        .domain_name("localhost")
        .ca_certificate(server_root_ca_cert)
        .identity(client_identity);

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
