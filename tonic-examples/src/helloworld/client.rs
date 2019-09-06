use tonic::transport::Channel;

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
}

use hello_world::{client::GreeterClient, HelloRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let origin = vec![http::Uri::from_static("http://[::1]:50051").into()];

    let svc = Channel::builder().balance_list(origin)?;

    let mut client = GreeterClient::new(svc);

    let request = tonic::Request::new(HelloRequest {
        name: "hello".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
