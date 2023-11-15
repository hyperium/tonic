use std::env;
use tonic::{transport::server::RoutesBuilder, transport::Server, Request, Response, Status};

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

fn init_echo(args: &[String], builder: &mut RoutesBuilder) {
    let enabled = args.iter().any(|arg| arg.as_str() == "echo");
    if enabled {
        println!("Adding Echo service...");
        let svc = EchoServer::new(MyEcho::default());
        builder.add_service(svc);
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

fn init_greeter(args: &[String], builder: &mut RoutesBuilder) {
    let enabled = args.iter().any(|arg| arg.as_str() == "greeter");

    if enabled {
        println!("Adding Greeter service...");
        let svc = GreeterServer::new(MyGreeter::default());
        builder.add_service(svc);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut routes_builder = RoutesBuilder::default();
    init_greeter(&args, &mut routes_builder);
    init_echo(&args, &mut routes_builder);

    let addr = "[::1]:50051".parse().unwrap();

    println!("Grpc server listening on {}", addr);

    Server::builder()
        .add_routes(routes_builder.routes())
        .serve(addr)
        .await?;

    Ok(())
}
