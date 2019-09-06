use tonic::{Request, Response, Server, Status};

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

#[derive(Default)]
pub struct MyGreeter {
    data: String,
}

#[tonic::async_trait]
impl hello_world::Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<hello_world::HelloRequest>,
    ) -> Result<Response<hello_world::HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let string = &self.data;

        println!("My data: {:?}", string);

        let reply = hello_world::HelloReply {
            message: "Zomg, it works!".into(),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    Server::builder()
        .serve(addr, hello_world::GreeterServer::new(greeter))
        .await?;

    Ok(())
}
