use tonic::{transport::Server, Request, Response, Status};

mod codec;
mod protos;

pub mod hello_world {
    include!(concat!(
        env!("OUT_DIR"),
        "/protobuf_codec/helloworld.Greeter.rs"
    ));
}
use crate::protos::helloworld::{HelloReply, HelloRequest};
use hello_world::greeter_server::{Greeter, GreeterServer};

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
            ..Default::default()
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
