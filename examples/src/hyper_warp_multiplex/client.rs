//! To hit the gRPC endpoint you must run this client via:
//! `cargo run --bin hyper-warp-multiplex-client
//! To hit the warp server you can run this command:
//! `curl localhost:50051/hello`

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

pub mod echo {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use echo::{echo_client::EchoClient, EchoRequest};
use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use hyper::{Client, Uri};

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

    let mut greeter_client = GreeterClient::new(add_origin);
    let mut echo_client = EchoClient::new(add_origin);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = greeter_client.say_hello(request).await?;

    println!("GREETER RESPONSE={:?}", response);

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = echo_client.unary_echo(request).await?;

    println!("ECHO RESPONSE={:?}", response);

    Ok(())
}
