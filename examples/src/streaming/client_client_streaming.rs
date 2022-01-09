pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

use pb::{echo_client::EchoClient, EchoRequest};

struct CountingStream {
    counter: i32,
}

impl Stream for CountingStream {
    type Item = EchoRequest;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.counter > 2 {
            return Poll::Pending;
        }

        let message = format!("foo {}", self.counter);
        self.get_mut().counter += 1;

        return Poll::Ready(Some(EchoRequest { message }));
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = EchoClient::connect("http://[::1]:50051").await.unwrap();

    let echo_response = client
        .client_streaming_echo(CountingStream { counter: 0 })
        .await
        .unwrap()
        .into_inner();

    println!("Connected...now sleeping for 2 seconds...");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    
    println!("It echoed: {}", echo_response.message);

    // Disconnect
    drop(echo_response);
    drop(client);

    println!("Disconnected...");

    Ok(())
}
