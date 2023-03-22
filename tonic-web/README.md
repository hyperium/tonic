# tonic-web

Enables tonic servers to handle requests from `grpc-web` clients directly,
without the need of an external proxy.

## Getting Started

```toml
[dependencies]
tonic-web = "<tonic-web-version>"
```

## Enabling tonic services

The easiest way to get started, is to call the function with your tonic service
and allow the tonic server to accept HTTP/1.1 requests:

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = GreeterServer::new(MyGreeter::default());

   Server::builder()
       .accept_http1(true)
       .layer(GrpcWebLayer::new())
       .add_service(greeter)
       .serve(addr)
       .await?;

   Ok(())
}
```

## Examples

See [the examples folder][example] for a server and client example.

[example]: https://github.com/hyperium/tonic/tree/master/examples/src/grpc-web
