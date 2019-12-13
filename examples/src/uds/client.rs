#[cfg(unix)]

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{greeter_client::GreeterClient, HelloRequest};
use http::Uri;
use std::convert::TryFrom;
use tokio::net::UnixStream;
use tonic::transport::Endpoint;
use tower::service_fn;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // We will ignore this uri because uds do not use it
    // if your connector does use the uri it will be provided
    // as the request to the `MakeConnection`.
    let channel = Endpoint::try_from("lttp://[::]:50051")?
        .connect_with_connector(service_fn(|_: Uri| {
            let path = "/tmp/tonic/helloworld";

            // Connect to a Uds socket
            UnixStream::connect(path)
        }))
        .await?;

    let mut client = GreeterClient::new(channel);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
