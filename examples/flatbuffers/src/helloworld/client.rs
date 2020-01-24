use hello_world::greeter_client::GreeterClient;
use hello_world::{HelloRequest, HelloRequestArgs};

pub mod hello_world {
    tonic::include_fbs!("helloworld");
}

mod shared;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request_payload = request("Tonic")?;
    println!("SENDING={:?}", request_payload.name()?);

    let request = tonic::Request::new(request_payload);
    let response = client.say_hello(request).await?;
    println!("RESPONSE={:?}", response.get_ref().message()?);

    Ok(())
}

fn request(name: &str) -> Result<HelloRequest<bytes::Bytes>, butte::Error> {
    let mut builder = butte::FlatBufferBuilder::new();
    let name = builder.create_string(name);
    let req = HelloRequest::create(&mut builder, &HelloRequestArgs { name });
    Ok(shared::build_into(builder, req)?)
}
