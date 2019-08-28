use hyper::Server;
use std::time::Duration;
use tokio::timer::Delay;
use tonic::{Request, Response, Status};

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

#[derive(Default, Clone)]
pub struct MyGreeter {
    data: String,
}

#[tonic::server(service = "helloworld.Greeter", proto = "hello_world")]
impl MyGreeter {
    pub async fn say_hello(
        &self,
        request: Request<hello_world::HelloRequest>,
    ) -> Result<Response<hello_world::HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let string = &self.data;

        let when = tokio::clock::now() + Duration::from_millis(100);
        Delay::new(when).await;

        println!("My data: {:?}", string);

        Delay::new(when).await;

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

    Server::bind(&addr)
        .http2_only(true)
        .serve(GreeterServer::new(greeter))
        .await?;

    Ok(())
}
