mod pb {
    tonic::include_proto!("helloworld");

    pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("helloworld_descriptor");
}

use tonic::{transport::Server, Request, Response, Result};

use pb::{greeter_server::Greeter, HelloReply, HelloRequest};

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(&self, request: Request<HelloRequest>) -> Result<Response<HelloReply>> {
        println!("Got a request from {:?}", request.remote_addr());

        let reply = HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(pb::FILE_DESCRIPTOR_SET)
        .build()
        .unwrap();

    let addr = "[::1]:50052".parse().unwrap();
    let greeter = MyGreeter::default();

    Server::builder()
        .add_service(service)
        .add_service(pb::greeter_server::GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
