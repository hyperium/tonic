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
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    let server = Server::builder().add_service(GreeterServer::new(greeter));

    match listenfd::ListenFd::from_env().take_tcp_listener(0)? {
        Some(listener) => {
            let listener = tokio_stream::wrappers::TcpListenerStream::new(
                tokio::net::TcpListener::from_std(listener)?,
            );

            server.serve_with_incoming(listener).await?;
        }
        None => {
            server.serve(addr).await?;
        }
    }

    Ok(())
}
