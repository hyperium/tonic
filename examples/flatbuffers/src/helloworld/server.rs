use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloReplyArgs, HelloRequest};

pub mod hello_world {
    tonic::include_fbs!("helloworld");
}

mod shared;

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest<bytes::Bytes>>,
    ) -> Result<Response<HelloReply<bytes::Bytes>>, Status> {
        let name = request.get_ref().name()?;

        println!(
            "Got a request from {:?} with name {:?}",
            request.remote_addr(),
            name
        );

        let mut builder = butte::FlatBufferBuilder::new();
        let message = builder.create_string(&format!("Hello {}!", name));
        let res = HelloReply::create(&mut builder, &HelloReplyArgs { message });
        Ok(Response::new(shared::build_into(builder, res)?))
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
