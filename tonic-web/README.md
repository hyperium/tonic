# tonic-web

Enables tonic servers to handle requests from `grpc-web` clients directly, without the need of an
external proxy. 

## Getting Started

 ```toml
 [dependencies]
 tonic_web = "0.1"
 ```

 ## Enabling tonic services

 The easiest way to get started, is to call the function with your tonic service and allow the tonic 
 server to accept HTTP/1.1 requests:

 ```rust
 #[tokio::main]
 async fn main() -> Result<(), Box<dyn std::error::Error>> {
     let addr = "[::1]:50051".parse().unwrap();
     let greeter = GreeterServer::new(MyGreeter::default());

     Server::builder()
        .accept_http1(true)
        .add_service(tonic_web::enable(greeter))
        .serve(addr)
        .await?;

    Ok(())
 }
 ```

## Examples

[tonic-web-demo][tonic-web-demo]: React+Typescript app that talking to a tonic-web enabled service using HTTP/1 or TLS.

[conduit][conduit]: An (in progress) implementation of the [realworld][realworld] demo in Tonic+Dart+Flutter. This app shows how
the same client implementation can talk to the same tonic-web enabled server using both `grpc` and `grpc-web` protocols
just by swapping the channel implementation. 

When the client is compiled for desktop, ios or android, a  grpc `ClientChannel` implementation is used.
When compiled for the web, a `GrpcWebClientChannel.xhr` implementation is used instead.``

[tonic-web-demo]: https://github.com/alce/tonic-web-demo
[conduit]: https://github.com/alce/conduit
[realworld]: https://github.com/gothinkster/realworld
