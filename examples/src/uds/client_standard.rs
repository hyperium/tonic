#![cfg_attr(not(unix), allow(unused_imports))]

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{greeter_client::GreeterClient, HelloRequest};

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Unix socket URI follows [RFC-3986](https://datatracker.ietf.org/doc/html/rfc3986)
    // which is aligned with [the gRPC naming convention](https://github.com/grpc/grpc/blob/master/doc/naming.md).
    // - unix:relative_path
    // - unix:///absolute_path
    let path = "unix:///tmp/tonic/helloworld";

    let mut client = GreeterClient::connect(path).await?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    panic!("The `uds` example only works on unix systems!");
}
