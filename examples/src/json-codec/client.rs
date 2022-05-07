//! A HelloWorld example that uses JSON instead of protobuf as the message serialization format.
//!
//! Generated code is the output of codegen as defined in the `build_json_codec_service` function
//! in the `examples/build.rs` file. As defined there, the generated code assumes that a module
//! `crate::common` exists which defines `HelloRequest`, `HelloResponse`, and `JsonCodec`.

pub mod common;
use common::HelloRequest;

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/json.helloworld.Greeter.rs"));
}
use hello_world::greeter_client::GreeterClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
