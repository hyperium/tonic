//! To hit the gRPC endpoint you must run this client via:
//! `cargo run --bin hyper-warp-client
//! To hit the warp server you can run this command:
//! `curl localhost:50051/hello`

use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use hyper::{Client, Uri};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder().http2_only(true).build_http();

    let uri = Uri::from_static("http://[::1]:50051");

    // Hyper's client requires that requests contain full Uris include a scheme and
    // an authority. Tonic's transport will handle this for you but when using the client
    // manually you need ensure the uri's are set correctly.
    let add_origin = tower::service_fn(|mut req: hyper::Request<tonic::body::BoxBody>| {
        let uri = Uri::builder()
            .scheme(uri.scheme().unwrap().clone())
            .authority(uri.authority().unwrap().clone())
            .path_and_query(req.uri().path_and_query().unwrap().clone())
            .build()
            .unwrap();

        *req.uri_mut() = uri;

        client.request(req)
    });

    let mut client = GreeterClient::new(add_origin);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
