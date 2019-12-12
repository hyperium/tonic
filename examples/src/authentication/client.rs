pub mod pb {
    tonic::include_proto!("grpc.examples.echo");
}

use http::header::HeaderValue;
use pb::{echo_client::EchoClient, EchoRequest};
use tonic::transport::Channel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::from_static("http://[::1]:50051")
        .intercept_headers(|headers| {
            headers.insert(
                "authorization",
                HeaderValue::from_static("Bearer some-secret-token"),
            );
        })
        .connect()
        .await?;

    let mut client = EchoClient::new(channel);

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = client.unary_echo(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
