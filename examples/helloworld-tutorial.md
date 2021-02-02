# Getting Started

This tutorial is meant to be an introduction to Tonic and assumes that you have basic [Rust] experience as well as an understanding of what [protocol buffers] are. If you don't, feel free to read up on the pages linked in this paragraph and come back to this tutorial once you feel you are ready!

[rust]: https://www.rust-lang.org/
[protocol buffers]: https://developers.google.com/protocol-buffers/docs/overview

## Prerequisites

To run the sample code and walk through the tutorial, the only prerequisite is Rust itself.
[rustup] is a convenient tool to install it, if you haven't already.

[rustup]: https://rustup.rs

## Project Setup

For this tutorial, we will start by creating a new Rust project with Cargo:

```shell
$ cargo new helloworld-tonic
$ cd helloworld-tonic
```

`tonic` works on rust `1.39` and above as it requires support for the `async_await`
feature.

```bash
$ rustup update
$ rustup component add rustfmt
```

## Defining the HelloWorld service

Our first step is to define the gRPC _service_ and the method _request_ and _response_ types using
[protocol buffers]. We will keep our `.proto` files in a directory in our crate's root.
Note that Tonic does not really care where our `.proto` definitions live.

```shell
$ mkdir proto
$ touch proto/helloworld.proto
```

Then you define RPC methods inside your service definition, specifying their request and response
types. gRPC lets you define four kinds of service methods, all of which are supported by Tonic. For this tutorial we will only use a simple RPC, if you would like to see a Tonic example which uses all four kinds please read the [routeguide tutorial].

[routeguide tutorial]: https://github.com/hyperium/tonic/blob/master/examples/routeguide-tutorial.md

First we define our package name, which is what Tonic looks for when including your protos in the client and server applications. Lets give this one a name of `helloworld`.

```proto
syntax = "proto3";
package helloworld;
```

Next we need to define our service. This service will contain the actual RPC calls we will be using in our application. An RPC contains an Identifier, a Request type, and returns a Response type. Here is our Greeter service, which provides the SayHello RPC method.

```proto
service Greeter {
    // Our SayHello rpc accepts HelloRequests and returns HelloReplies
    rpc SayHello (HelloRequest) returns (HelloReply);
}
```

Finally, we have to actually define those types we used above in our `SayHello` RPC method. RPC types are defined as messages which contain typed fields. Here is what that will look like for our HelloWorld application:

```proto
message HelloRequest {
    // Request message contains the name to be greeted
    string name = 1;
}

message HelloReply {
    // Reply contains the greeting message
    string message = 1;
}
```

Great! Now our `.proto` file should be complete and ready for use in our application. Here is what it should look like completed:

```proto
syntax = "proto3";
package helloworld;

service Greeter {
    rpc SayHello (HelloRequest) returns (HelloReply);
}

message HelloRequest {
   string name = 1;
}

message HelloReply {
    string message = 1;
}
```

## Application Setup

Now that have defined the protobuf for our application we can start writing our application with Tonic! Let's first add our required dependencies to the `Cargo.toml`.

```toml
[package]
name = "helloworld-tonic"
version = "0.1.0"
edition = "2018"

[[bin]] # Bin to run the HelloWorld gRPC server
name = "helloworld-server"
path = "src/server.rs"

[[bin]] # Bin to run the HelloWorld gRPC client
name = "helloworld-client"
path = "src/client.rs"

[dependencies]
tonic = "0.4"
prost = "0.7"
tokio = { version = "1.0", features = ["macros", "rt-multi-thread"] }

[build-dependencies]
tonic-build = "0.4"
```

We include `tonic-build` as a useful way to incorporate the generation of our client and server gRPC code into the build process of our application. We will setup this build process now:

## Generating Server and Client code

At the root of your crate, create a `build.rs` file and add the following code:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/helloworld.proto")?;
    Ok(())
}
```

This tells `tonic-build` to compile your protobufs when you build your Rust project. While you can configure this build process in a number of ways, we will not get into the details in this introductory tutorial. Please see the [tonic-build] documentation for details on configuration.

[tonic-build]: https://github.com/hyperium/tonic/blob/master/tonic-build/README.md

## Writing our Server

Now that the build process is written and our dependencies are all setup, we can begin writing the fun stuff! We need to import the things we will be using in our server, including the protobuf. Start by making a file called `server.rs` in your `/src` directory and writing the following code:

```rust
use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

pub mod hello_world {
    tonic::include_proto!("helloworld"); // The string specified here must match the proto package name
}
```

Next up, let's implement the Greeter service we previously defined in our `.proto` file. Here's what that might look like:

```rust
#[derive(Debug, Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>, // Accept request of type HelloRequest
    ) -> Result<Response<HelloReply>, Status> { // Return an instance of type HelloReply
        println!("Got a request: {:?}", request);

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name).into(), // We must use .into_inner() as the fields of gRPC requests and responses are private
        };

        Ok(Response::new(reply)) // Send back our formatted greeting
    }
}
```

Finally, let's define the Tokio runtime that our server will actually run on. This requires Tokio to be added as a dependency, so make sure you included that!

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let greeter = MyGreeter::default();

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
```

Altogether your server should look something like this once you are done:

```rust
use tonic::{transport::Server, Request, Response, Status};

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[derive(Debug, Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        println!("Got a request: {:?}", request);

        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name).into(),
        };

        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let greeter = MyGreeter::default();

    Server::builder()
        .add_service(GreeterServer::new(greeter))
        .serve(addr)
        .await?;

    Ok(())
}
```

You should now be able to run your HelloWorld gRPC server using the command `cargo run --bin helloworld-server`. This uses the [[bin]] we defined earlier in our `Cargo.toml` to run specifically the server. 

If have a gRPC GUI client such as [Bloom RPC] you should be able to send requests to the server and get back greetings!

Or if you use [grpcurl] then you can simply try send requests like this:
```
$ grpcurl -plaintext -import-path ./proto -proto helloworld.proto -d '{"name": "Tonic"}' [::]:50051 helloworld.Greeter/SayHello
```
And receiving responses like this:
```
{
  "message": "Hello Tonic!"
}
```

[bloom rpc]: https://github.com/uw-labs/bloomrpc
[grpcurl]: https://github.com/fullstorydev/grpcurl

## Writing our Client

So now we have a running gRPC server, and that's great but how can our application communicate with it? This is where our client would come in. Tonic supports both client and server implementations. Similar to the server, we will start by creating a file `client.rs` in our `/src` directory and importing everything we will need:

```rust
use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}
```

The client is much simpler than the server as we don't need to implement any service methods, just make requests. Here is a Tokio runtime which will make our request and print the response to your terminal:

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
```

That's it! Our complete client file should look something like below, if it doesn't please go back and make sure you followed along correctly:

```rust
use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
```

## Putting it all together

At this point we have written our protobuf file, a build file to compile our protobufs, a server which implements our SayHello service, and a client which makes requests to our server. You should have a `proto/helloworld.proto` file, a `build.rs` file at the root of your project, and `src/server.rs` as well as a `src/client.rs` files.

To run the server, run `cargo run --bin helloworld-server`.
To run the client, run `cargo run --bin helloworld-client` in another terminal window.

You should see the request logged out by the server in its terminal window, as well as the response logged out by the client in its window.

Congrats on making it through this introductory tutorial! We hope that this walkthrough tutorial has helped you understand the basics of Tonic, and how to get started writing high-performance, interoperable, and flexible gRPC servers in Rust. For a more in-depth tutorial which showcases an advanced gRPC server in Tonic, please see the [routeguide tutorial].
