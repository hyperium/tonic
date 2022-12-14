pub mod hello_world {
    tonic::include_proto!("helloworld");
}

pub mod echo {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use echo::{echo_client::EchoClient, EchoRequest};
use hello_world::{greeter_client::GreeterClient, HelloRequest};
use tonic::transport::Endpoint;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await?;

    let mut greeter_client = GreeterClient::new(channel.clone());
    let mut echo_client = EchoClient::new(channel);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = greeter_client.say_hello(request).await?;

    println!("GREETER RESPONSE={:?}", response);

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = echo_client.unary_echo(request).await?;

    println!("ECHO RESPONSE={:?}", response);

    Ok(())
}
