use tonic::{transport::Server, Code, Request, Response, Status};
use tonic_types::{BadRequest, Help, LocalizedMessage, StatusExt};

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

        // Extract request data
        let name = request.into_inner().name;

        // Create empty BadRequest struct
        let mut bad_request = BadRequest::new(vec![]);

        // Add violations conditionally
        if name.is_empty() {
            bad_request.add_violation("name", "name cannot be empty");
        } else if name.len() > 20 {
            bad_request.add_violation("name", "name is too long");
        }

        if !bad_request.is_empty() {
            // Add aditional error details if necessary
            let help = Help::with_link("description of link", "https://resource.example.local");

            let localized_message = LocalizedMessage::new("en-US", "message for the user");

            // Generate error status
            let status = Status::with_error_details_vec(
                Code::InvalidArgument,
                "request contains invalid arguments",
                vec![bad_request.into(), help.into(), localized_message.into()],
            );

            return Err(status);
        }

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", name),
        };
        Ok(Response::new(reply))
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
