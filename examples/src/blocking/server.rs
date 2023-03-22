use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

use tokio::runtime::Runtime;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[derive(Debug, Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };

        Ok(Response::new(reply))
    }
}

fn main() {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    let rt = Runtime::new().expect("failed to obtain a new RunTime object");
    let server_future = Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(addr);
    rt.block_on(server_future)
        .expect("failed to successfully run the future on RunTime");
}
