//! A HelloWorld example that uses a custom codec instead of the default Prost codec.
//!
//! Generated code is the output of codegen as defined in the `examples/build.rs` file.
//! The generation is the one with .codec_path("crate::common::SmallBufferCodec")
//! The generated code assumes that a module `crate::common` exists which defines
//! `SmallBufferCodec`, and `SmallBufferCodec` must have a Default implementation.

pub mod common;

pub mod small_buf {
    include!(concat!(env!("OUT_DIR"), "/smallbuf/helloworld.rs"));
}
use small_buf::greeter_client::GreeterClient;

use crate::small_buf::HelloRequest;

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
