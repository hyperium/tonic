pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use pb::{echo_client::EchoClient, EchoRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = EchoClient::connect("http://[::1]:50051").await.unwrap();

    let mut stream = client
        .server_streaming_echo(EchoRequest {
            message: "foo".into(),
        })
        .await
        .unwrap().into_inner();

    println!("Connected...now sleeping for 2 seconds...");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let incoming_message = stream.message().await;
    match incoming_message {
        Ok(Some(echo)) => {
            println!("It echoed: {}", echo.message);
        }
        Ok(None) => {
            println!("No message passed");
        }
        Err(status) => {
            println!("Received status {}", status);
        }
    }

    // Disconnect
    drop(stream);
    drop(client);

    println!("Disconnected...");

    Ok(())
}
