use std::env;
use tonic::{transport::server::Routes, transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

use echo::echo_server::{Echo, EchoServer};
use echo::{EchoRequest, EchoResponse};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

pub mod echo {
    tonic::include_proto!("grpc.examples.unaryecho");
}

type EchoResult<T> = Result<Response<T>, Status>;

#[derive(Default)]
pub struct MyEcho {}

#[tonic::async_trait]
impl Echo for MyEcho {
    async fn unary_echo(&self, request: Request<EchoRequest>) -> EchoResult<EchoResponse> {
        println!("Got an echo request from {:?}", request.remote_addr());

        let message = format!("you said: {}", request.into_inner().message);

        Ok(Response::new(EchoResponse { message }))
    }
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        println!("Got a greet request from {:?}", request.remote_addr());

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let routes_builder = Routes::builder();

    let routes_builder = if args.iter().any(|arg| arg.as_str() == "greeter") {
        println!("Adding Greeter service...");
        routes_builder.add_service(GreeterServer::new(MyGreeter::default()))
    } else {
        routes_builder
    };

    let routes_builder = if args.iter().any(|arg| arg.as_str() == "echo") {
        println!("Adding Echo service...");
        routes_builder.add_service(EchoServer::new(MyEcho::default()))
    } else {
        routes_builder
    };

    let routes = routes_builder.build();

    let addr = "[::1]:50051".parse().unwrap();

    println!("Grpc server listening on {}", addr);

    Server::builder().add_routes(routes).serve(addr).await?;

    Ok(())
}
