use tonic::transport::Server;
use tonic::{Request, Response, Status};

mod proto {
    tonic::include_proto!("helloworld");

    pub(crate) const HELLO_WORLD_DESCRIPTOR_SET: &'static [u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/helloworld_descriptor.bin"));
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl proto::greeter_server::Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<proto::HelloRequest>,
    ) -> Result<Response<proto::HelloReply>, Status> {
        println!("Got a request from {:?}", request.remote_addr());

        let reply = proto::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(proto::HELLO_WORLD_DESCRIPTOR_SET)
        .build()
        .unwrap();

    let addr = "[::1]:50052".parse().unwrap();
    let greeter = MyGreeter::default();

    Server::builder()
        .add_service(reflection_service)
        .add_service(proto::greeter_server::GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
