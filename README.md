<p align="center">
  <img src="https://github.com/LucioFranco/tonic/raw/master/.github/assets/tonic_ghbanner.png" alt="Vector" style="width:60%;">
</p>

A rust implementation of [gRPC], a high performance, open source, general
RPC framework that puts mobile and HTTP/2 first.

[`tonic`] is a gRPC over HTTP/2 implementation focused on high performance, interoperability, and flexibility. This library was created to have first class support of async/await and to act as a core building block for production systems written in Rust.

[![Crates.io](https://img.shields.io/crates/v/tonic)](https://crates.io/crates/tonic)
[![Documentation](https://docs.rs/tonic/badge.svg)](https://docs.rs/tracing)
[![Crates.io](https://img.shields.io/crates/l/tonic)](LICENSE)


[Examples] | [Website] | [Docs] | [Chat]

## Overview

[`tonic`] is composed of three main components the generic gRPC implementation, the high performance HTTP/2
implementation and the codegen powered by [`prost`]. The generic implementation can support any HTTP/2
implementation and any encoding via a set of generic traits. The HTTP/2 implementation is based on [`hyper`]
which is a fast HTTP/1.1 and HTTP/2 client and server built on top of the robust [`tokio`] stack. The codegen
contains the tools to build clients and servers from [`protobuf`] definitions.

## Features

- Bi-directional streaming
- High performance async io
- Interoperability
- TLS backed via either [`openssl`] or [`rustls`]
- Load balancing
- Custom metadata
- Authentication

## Getting Started

Examples can be found in [`tonic-examples`] and for more complex scenarios [`tonic-interop`]
may be a good resource as it shows examples of many of the gRPC features.

### Examples

#### Rust Version

`tonic` currently works on rust `1.39-beta` and above as it requires support for the `async_await`
feature. To install the beta simply follow the commands below:

```bash
$ rustup install beta && rustup component add rustfmt --toolchain beta
$ cargo +beta build
```

#### Client

```rust
pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

use hello_world::{client::GreeterClient, HelloRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051")?;

    let request = tonic::Request::new(HelloRequest {
        name: "hello".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
```

#### Server

```rust
use tonic::{transport::Server, Request, Response, Status};

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

use hello_world::{
    server::{Greeter, GreeterServer},
    HelloReply, HelloRequest,
};

#[derive(Default)]
pub struct MyGreeter {
    data: String,
}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
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
        .serve(addr, GreeterServer::new(greeter))
        .await?;

    Ok(())
}
```

## Getting Help

First, see if the answer to your question can be found in the API documentation.
If the answer is not there, there is an active community in
the [Tonic Discord channel][chat]. We would be happy to try to answer your
question.  Last, if that doesn't work, try opening an [issue] with the question.

[chat]: https://discord.gg/6yGkFeN
[issue]: https://github.com/hyperium/tonic/issues/new

## Project Layout

- [`tonic`](https://github.com/hyperium/tonic/tree/master/tonic): Generic gRPC and HTTP/2 client/server
implementation.
- [`tonic-build`](https://github.com/hyperium/tonic/tree/master/tonic): [`prost`] based service codegen.

## Contributing

:balloon: Thanks for your help improving the project! We are so happy to have
you! We have a [contributing guide][guide] to help you get involved in the Tracing
project.

[guide]: CONTRIBUTING.md

## License

This project is licensed under the [MIT license](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Tracing by you, shall be licensed as MIT, without any additional
terms or conditions.


[gRPC]: https://grpc.io
[`tonic`]: https://github.com/hyperium/tonic
[`tokio`]: https://github.com/tokio-rs/tokio
[`hyper`]: https://github.com/hyperium/hyper
[`prost`]: https://github.com/danburkert/prost
[`protobuf`]: https://developers.google.com/protocol-buffers
[`rustls`]: https://github.com/ctz/rustls
[`openssl`]: https://www.openssl.org/
[Examples]: https://github.com/hyperium/tonic/tree/master/tonic-examples
[Website]: https://tokio.rs
[Docs]: https://docs.rs/tonic
[Chat]: https://discord.gg/6yGkFeN
