//! Client for the UDS socket-activation example.
#![cfg_attr(not(unix), allow(unused_imports))]

use hello_world::HelloRequest;
use hello_world::greeter_client::GreeterClient;
use hyper_util::rt::TokioIo;
#[cfg(unix)]
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("TONIC_UDS_PATH")
        .unwrap_or_else(|_| "/tmp/tonic/socket-activation.sock".to_string());

    // The URI is ignored by the custom connector; UDS uses the path below.
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(path).await?)) }
        }))
        .await?;

    let mut client = GreeterClient::new(channel);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={response:?}");

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    panic!("The socket-activation UDS example only works on unix systems!");
}
