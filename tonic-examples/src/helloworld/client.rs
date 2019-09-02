use tonic::transport::Channel;

pub mod hello_world {
    include!(concat!(env!("OUT_DIR"), "/helloworld.rs"));
    tonic::client!(service = "helloworld.Greeter", proto = "self");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let origin = vec![
        http::Uri::from_static("http://[::1]:50051").into(),

    ];

    let svc = Channel::builder().balance_list(origin)?;

    let mut client = hello_world::GreeterClient::new(svc);

    let request = tonic::Request::new(hello_world::HelloRequest {
        name: "hello".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
