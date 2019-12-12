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
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(|dst: Uri| {
            let host = dst.host().unwrap();
            let port = dst.port().unwrap();

            // Connect to a Uds socket
            UnixStream::connect(format!("{}:{}", host, port))
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
