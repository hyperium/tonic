pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use pb::{echo_client::EchoClient, EchoRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = EchoClient::connect("http://[::1]:50051").await.unwrap();

    let stream = client
        .server_streaming_echo(EchoRequest {
            message: "foo".into(),
        })
        .await
        .unwrap();

    println!("Connected...now sleeping for 2 seconds...");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Disconnect
    drop(stream);
    drop(client);

    println!("Disconnected...");

    Ok(())
}
