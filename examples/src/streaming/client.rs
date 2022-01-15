pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;

use pb::{echo_client::EchoClient, EchoRequest};

struct CountingStream {
    counter: u32,
    count_to: u32,
}

impl CountingStream {

   pub fn new(count_to: u32) -> CountingStream {
        CountingStream {
            counter: 0,
            count_to,
        }
    }
}

impl Stream for CountingStream {
    type Item = EchoRequest;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.counter >= self.count_to {
            return Poll::Pending;
        }

        let message = format!("foo {}", self.counter);
        self.get_mut().counter += 1;

        return Poll::Ready(Some(EchoRequest { message }));
    }
}

async fn run_server_streaming(client: &mut EchoClient<tonic::transport::Channel>) {
    let mut stream = client
        .server_streaming_echo(EchoRequest {
            message: "foo".into(),
        })
        .await
        .unwrap().into_inner();

    let incoming_message = stream.message().await;
    match incoming_message {
        Ok(Some(echo_response)) => {
            println!("RESPONSE={:?}", echo_response.message);
        }
        Ok(None) => {
            println!("No response received");
        }
        Err(status) => {
            println!("Received status {}", status);
        }
    }
}

async fn run_client_streaming(client: &mut EchoClient<tonic::transport::Channel>) {
    let echo_response = client
        .client_streaming_echo(CountingStream::new(1))
        .await
        .unwrap()
        .into_inner();

    println!("RESPONSE={:?}", echo_response.message);
}

async fn run_bidirectional_streaming(client: &mut EchoClient<tonic::transport::Channel>) {
    let mut stream = client
        .bidirectional_streaming_echo(CountingStream::new(3))
        .await
        .unwrap()
        .into_inner();

    let mut response_counter = 0;
    while response_counter < 3 {
        let incoming_message = stream.message().await;
        match incoming_message {
            Ok(Some(echo_response)) => {
                println!("RESPONSE={:?}", echo_response.message);
            }
            Ok(None) => {
                println!("No response received");
                break;
            }
            Err(status) => {
                println!("Received status {}", status);
                break;
            }
        }
        response_counter += 1;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut client = EchoClient::connect("http://[::1]:50051").await.unwrap();

    println!("\n*** SERVER STREAMING ***");
    run_server_streaming(&mut client).await;

    println!("\n*** CLIENT STREAMING ***");
    run_client_streaming(&mut client).await;

    println!("\n*** BIDIRECTIONAL STREAMING ***");
    run_bidirectional_streaming(&mut client).await;

    // Disconnect
    drop(client);

    println!("\nDisconnected...");

    Ok(())
}
