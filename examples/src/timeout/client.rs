//! A client that demonstrates gRPC request timeouts.
//!
//! Shows two timeout mechanisms:
//! 1. **Server-side timeout** — set via `Server::builder().timeout()` on the server.
//!    The server enforces the deadline and returns CANCELLED if exceeded.
//! 2. **Client-side deadline** — set via `Request::set_timeout()`, which sends the
//!    `grpc-timeout` header. The server respects the shorter of the two deadlines.
//!
//! ```not_rust
//! # Start the timeout server first:
//! cargo run --bin timeout-server
//!
//! # Then in another terminal:
//! cargo run --bin timeout-client
//! ```

use std::time::Duration;

use hello_world::HelloRequest;
use hello_world::greeter_client::GreeterClient;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    // 1. Fast request — completes within the server's 2s timeout.
    println!("--- Fast request ---");
    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });
    match client.say_hello(request).await {
        Ok(response) => println!("RESPONSE = {:?}", response.into_inner().message),
        Err(status) => println!("ERROR = {status}"),
    }

    // 2. Slow request — exceeds the server's 2s timeout.
    println!("\n--- Slow request (server timeout) ---");
    let request = tonic::Request::new(HelloRequest {
        name: "slow-request".into(),
    });
    match client.say_hello(request).await {
        Ok(response) => println!("RESPONSE = {:?}", response.into_inner().message),
        Err(status) => println!("ERROR = {status}"),
    }

    // 3. Client-side deadline — the client sets a 1s deadline via grpc-timeout header,
    //    which is shorter than the server's 2s timeout.
    println!("\n--- Fast request with short client deadline (1s) ---");
    let mut request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });
    request.set_timeout(Duration::from_secs(1));
    match client.say_hello(request).await {
        Ok(response) => println!("RESPONSE = {:?}", response.into_inner().message),
        Err(status) => println!("ERROR = {status}"),
    }

    Ok(())
}
