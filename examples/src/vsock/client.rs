#![cfg_attr(not(unix), allow(unused_imports))]

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{greeter_client::GreeterClient, HelloRequest};
use tokio_vsock::VsockStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

// Use vsock-loopback so we don't need to spin up a VM. Must match server.
static TEST_CID: u32 = vsock::VMADDR_CID_LOCAL;
// Arbitrarily chosen. Must match server.
static TEST_PORT: u32 = 8000;

// Virtio VSOCK does not use URIs, hence this URI will never be used.
// It is defined purely since in order to create a channel, since a URI has to
// be supplied to create an `Endpoint`.
static IGNORED_ENDPOINT_URI: &str = "file://[::]:0";

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::try_from(IGNORED_ENDPOINT_URI)?
        .connect_with_connector(service_fn(|_: Uri| {
            VsockStream::connect(TEST_CID, TEST_PORT)
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

#[cfg(not(unix))]
fn main() {
    panic!("The `uds` example only works on unix systems!");
}
