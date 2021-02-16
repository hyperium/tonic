use tonic::transport::Server;
use tonic::{Request, Response, Status};

mod proto {
    tonic::include_proto!("helloworld");

    pub(crate) const FILE_DESCRIPTOR_SET: &'static [u8] =
        tonic::include_file_descriptor_set!("helloworld_descriptor");
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
    let service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
        .build()
        .unwrap();

    let addr = "[::1]:50052".parse().unwrap();
    let greeter = MyGreeter::default();

    Server::builder()
        .add_service(service)
        .add_service(proto::greeter_server::GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
