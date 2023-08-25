use hello_world::{greeter_client::GreeterClient, HelloRequest};
use tonic_web::GrpcWebClientLayer;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Must use hyper directly...
    let client = hyper::Client::builder().build_http();

    let svc = tower::ServiceBuilder::new()
        .layer(GrpcWebClientLayer::new())
        .service(client);

    let mut client = GreeterClient::with_origin(svc, "http://127.0.0.1:3000".try_into()?);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
