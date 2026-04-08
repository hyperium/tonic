//! Example: standalone gRPC greeter server for testing xDS.
//!
//! Starts a greeter backend on a given port. Used together with the
//! `xds_server` and `channel` examples.
//!
//! # Quick start
//!
//! ```sh
//! ./tonic-xds/examples/run_xds_example.sh
//! ```
//!
//! # Running individually
//!
//! ```sh
//! # Start on port 50051 (default):
//! cargo run -p tonic-xds --example greeter_server --features testutil
//!
//! # Custom port:
//! PORT=50052 cargo run -p tonic-xds --example greeter_server --features testutil
//! ```

use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tonic_xds::testutil::proto::helloworld::{
    HelloReply, HelloRequest,
    greeter_server::{Greeter, GreeterServer},
};

struct MyGreeter {
    addr: String,
}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        let name = request.into_inner().name;
        println!("Received request: name={name}");
        Ok(Response::new(HelloReply {
            message: format!("Hello {name} from {}", self.addr),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "50051".to_string());
    let addr: std::net::SocketAddr = format!("0.0.0.0:{port}").parse()?;

    println!("Greeter server listening on {addr}");

    Server::builder()
        .add_service(GreeterServer::new(MyGreeter {
            addr: addr.to_string(),
        }))
        .serve(addr)
        .await?;

    Ok(())
}
