use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use tonic::codec::CompressionEncoding;
use tonic::transport::Channel;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::builder("http://[::1]:50051".parse().unwrap())
        .connect()
        .await
        .unwrap();

    let mut client = GreeterClient::new(channel)
        .send_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Gzip);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    dbg!(response);

    Ok(())
}
