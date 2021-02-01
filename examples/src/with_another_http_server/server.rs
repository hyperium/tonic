use tonic::{transport::Server, Request, Response, Status};
use warp::Filter;

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
    let grpc_addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();
    let tonic = Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(grpc_addr);

    // with an http server started on the same host as well
    let http_addr: std::net::SocketAddr = "[::1]:3030".parse().unwrap();
    let routes = warp::any().map(|| "Hello, World!");
    let http = warp::serve(routes).run(http_addr);

    println!("Grpc GreeterServer listening on {}", grpc_addr);
    println!("Http Server listening on {}", http_addr);
    let (tonic_serve_result, _) = tokio::join!(tonic, http);
    tonic_serve_result?;

    Ok(())
}

/*
Under examples folder, try it as:

grpcurl -plaintext -import-path ./proto -proto helloworld/helloworld.proto -d '{"name": "Tonic"}' [::]:50051 helloworld.Greeter/SayHello

curl localhost:3030
*/
