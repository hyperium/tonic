pub mod pb {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use pb::{EchoRequest, echo_client::EchoClient};
use tonic::{Request, metadata::MetadataValue, transport::Channel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Channel::from_static("http://[::1]:50051").connect().await?;

    let token: MetadataValue<_> = "Bearer some-auth-token".parse()?;

    let mut client = EchoClient::with_interceptor(channel, move |mut req: Request<()>| {
        req.metadata_mut().insert("authorization", token.clone());
        Ok(req)
    });

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = client.unary_echo(request).await?;

    println!("RESPONSE={response:?}");

    Ok(())
}
