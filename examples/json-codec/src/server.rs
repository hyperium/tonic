//! A HelloWorld example that uses JSON instead of protobuf as the message serialization format.
//!
//! Generated code is the output of codegen as defined in the `build_json_codec_service` function
//! in the `examples/build.rs` file. As defined there, the generated code assumes that a module
//! `crate::common` exists which defines `HelloRequest`, `HelloResponse`, and `JsonCodec`.

use tonic::{transport::Server, Request, Response, Status};

pub mod common;
use common::{HelloRequest, HelloResponse};

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/json.helloworld.Greeter.rs"));
}
use hello_world::greeter_server::{Greeter, GreeterServer};

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloResponse>, Status> {
        println!("Got a request from {:?}", request.remote_addr());

        let reply = HelloResponse {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
