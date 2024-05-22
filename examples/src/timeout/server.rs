use std::time::Duration;
use tokio::time::sleep;
use tonic::transport::server::Routes;
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

        sleep(Duration::from_millis(5000)).await;

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let routes = Routes::builder()
        .add_service(GreeterServer::new(MyGreeter::default()))
        .build();

    println!("GreeterServer listening on {}", addr);

    Server::builder().add_routes(routes).serve(addr).await?;

    Ok(())
}
