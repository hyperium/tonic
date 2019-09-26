pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

use hello_world::{client::GreeterClient, HelloRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051")?;

    let req = HelloRequest { name: "hello".into() };
    let res = client.say_hello(req).await?;

    println!("RESPONSE={:?}", res);

    Ok(())
}
