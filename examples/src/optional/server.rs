use std::env;
use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        println!("Got a request from {:?}", request.remote_addr());

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let enabled = args.get(1) == Some(&"enable".to_string());

    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    let optional_service = if enabled {
        println!("MyGreeter enabled");
        Some(GreeterServer::new(greeter))
    } else {
        println!("MyGreeter disabled");
        None
    };

    println!("GreeterServer listening on {}", addr);

    Server::builder()
        .add_optional_service(optional_service)
        .serve(addr)
        .await?;

    Ok(())
}
