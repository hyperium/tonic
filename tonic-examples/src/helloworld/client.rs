#![feature(async_await)]

use tokio::net::TcpStream;
use tower_h2::{add_origin::AddOrigin, Connection};

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
    tonic::client!(service = "helloworld.Greeter", proto = "self");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let io = TcpStream::connect(&addr).await?;

    let origin = http::Uri::from_shared(format!("http://{}", addr).into()).unwrap();

    let svc = Connection::handshake(io).await?;
    let svc = AddOrigin::new(svc, origin);

    let mut client = hello_world::GreeterClient::new(svc);

    let request = tonic::Request::new(hello_world::HelloRequest {
        name: "hello".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
