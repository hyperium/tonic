#![cfg_attr(not(windows), allow(unused_imports))]

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{greeter_client::GreeterClient, HelloRequest};

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Named pipe URI follows [RFC-3986](https://datatracker.ietf.org/doc/html/rfc3986)
    // which is aligned with [the gRPC naming convention]

    let pipe_name = r"\\.\pipe\tonic\helloworld";
    let mut client = GreeterClient::connect(pipe_name).await?;
    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });
    let response = client.say_hello(request).await?;
    println!("RESPONSE={response:?}");
    Ok(())
}

#[cfg(not(windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    unimplemented!("Named pipes are only supported on Windows");
}
