//! A server that enforces a request timeout using `Server::builder().timeout()`.
//!
//! The `say_hello` handler sleeps for a configurable duration, allowing you to
//! test what happens when a request exceeds the server-side timeout.
//!
//! ```not_rust
//! cargo run --bin timeout-server
//! ```

use std::time::Duration;

use tokio::time::sleep;
use tonic::{Request, Response, Status, transport::Server};

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
        let name = request.into_inner().name;
        println!("Got a request for: {name}");

        // Simulate slow processing for names starting with "slow".
        if name.starts_with("slow") {
            println!("Simulating slow processing (3s)...");
            sleep(Duration::from_secs(3)).await;
        }

        let reply = HelloReply {
            message: format!("Hello {name}!"),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {addr}");
    println!("Server-side timeout: 2 seconds");

    Server::builder()
        // Enforce a 2-second timeout on all request handlers.
        // If a handler takes longer, the client receives a CANCELLED status.
        .timeout(Duration::from_secs(2))
        .add_service(GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
