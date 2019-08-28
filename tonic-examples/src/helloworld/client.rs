use hyper::client::conn::Builder;
use hyper::client::connect::HttpConnector;
use hyper::client::service::{Connect, MakeService};
use tonic::service::add_origin::AddOrigin;

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
    tonic::client!(service = "helloworld.Greeter", proto = "self");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let origin = http::Uri::from_static("http://[::1]:50051");

    let settings = Builder::new().http2_only(true).clone();
    let mut maker = Connect::new(HttpConnector::new(1), settings);

    let svc = maker.make_service(origin.clone()).await?;

    let svc = AddOrigin::new(svc, origin);

    let mut client = hello_world::GreeterClient::new(svc);

    let request = tonic::Request::new(hello_world::HelloRequest {
        name: "hello".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
