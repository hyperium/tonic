//! A HelloWorld example that uses a custom codec instead of the default Prost codec.
//!
//! Generated code is the output of codegen as defined in the `examples/build.rs` file.
//! The generation is the one with .codec_path("crate::common::SmallBufferCodec")
//! The generated code assumes that a module `crate::common` exists which defines
//! `SmallBufferCodec`, and `SmallBufferCodec` must have a Default implementation.

use tonic::{transport::Server, Request, Response, Status};

pub mod common;

pub mod small_buf {
    include!(concat!(env!("OUT_DIR"), "/smallbuf/helloworld.rs"));
}
use small_buf::{
    greeter_server::{Greeter, GreeterServer},
    HelloReply, HelloRequest,
};

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        println!("Got a request from {:?}", request.remote_addr());

        let reply = HelloReply {
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
