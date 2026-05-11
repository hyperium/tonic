#[allow(unused)]
mod generated {
    pub mod helloworld {
        include!("generated/generated.rs"); // Contains messages
        include!("generated/helloworld_grpc.pb.rs"); // Contains grpc stubs
    }
}

use std::env;
use std::sync::Arc;

use generated::helloworld::HelloRequest;
use generated::helloworld::greeter_client::GreeterClient;
use grpc::client::Channel;
use grpc::client::ChannelOptions;
use grpc::credentials::LocalChannelCredentials;
use protobuf::proto;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let name = if args.len() > 1 {
        args[1].clone()
    } else {
        "Rust World".to_owned()
    };

    // Create a new gRPC channel:
    let channel = Channel::new(
        "dns:///[::1]:50051",
        Arc::new(LocalChannelCredentials::new()),
        ChannelOptions::default(),
    );
    let client = GreeterClient::new(channel);

    // Send the request and print the response:
    let request = proto!(HelloRequest { name });
    let response = client
        .say_hello(request.as_view())
        .await
        .expect("RPC error");

    println!("Greeting: {:}", response.message());
}
