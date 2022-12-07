use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use std::time::Duration;
use tower::timeout::Timeout;

use tonic::transport::Channel;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::from_static("http://[::1]:50051").connect().await?;
    let timeout_channel = Timeout::new(channel, Duration::from_millis(1000));

    let mut client = GreeterClient::new(timeout_channel);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
