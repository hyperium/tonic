#![cfg_attr(not(unix), allow(unused_imports))]

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{
    greeter_server::{Greeter, GreeterServer},
    HelloReply, HelloRequest,
};
use tokio_vsock::VsockListener;
use tonic::transport::server::VsockConnectInfo;
use tonic::{transport::Server, Request, Response, Status};

// Use vsock-loopback so we don't need to spin up a VM.
static TEST_CID: u32 = vsock::VMADDR_CID_LOCAL;
// Arbitrarily chosen. Must match client.
static TEST_PORT: u32 = 8000;

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        #[cfg(unix)]
        {
            let conn_info = request.extensions().get::<VsockConnectInfo>().unwrap();
            println!("Got a request {:?} with info {:?}", request, conn_info);
        }

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let greeter = MyGreeter::default();

    let stream = VsockListener::bind(TEST_CID, TEST_PORT)?.incoming();

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve_with_incoming(stream)
        .await?;

    Ok(())
}

#[cfg(not(unix))]
fn main() {
    panic!("The `uds` example only works on unix systems!");
}
