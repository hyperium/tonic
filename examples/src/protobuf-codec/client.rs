mod codec;
mod protos;

pub mod hello_world {
    include!(concat!(
        env!("OUT_DIR"),
        "/protobuf_codec/helloworld.Greeter.rs"
    ));
}
use crate::protos::helloworld::HelloRequest;
use hello_world::greeter_client::GreeterClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
        ..Default::default()
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
