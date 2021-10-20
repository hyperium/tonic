use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;
use tonic::{
    codegen::InterceptedService,
    service::Interceptor,
    transport::{Channel, Endpoint},
    Request, Status,
};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await?;

    let mut client = GreeterClient::with_interceptor(channel, intercept);

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}

/// This function will get called on each outbound request. Returning a
/// `Status` here will cancel the request and have that status returned to
/// the caller.
fn intercept(req: Request<()>) -> Result<Request<()>, Status> {
    println!("Intercepting request: {:?}", req);
    Ok(req)
}

// You can also use the `Interceptor` trait to create an interceptor type
// that is easy to name
struct MyInterceptor;

impl Interceptor for MyInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        Ok(request)
    }
}

#[allow(dead_code, unused_variables)]
async fn using_named_interceptor() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await?;

    let client: GreeterClient<InterceptedService<Channel, MyInterceptor>> =
        GreeterClient::with_interceptor(channel, MyInterceptor);

    Ok(())
}

// Using a function pointer type might also be possible if your interceptor is a
// bare function that doesn't capture any variables
#[allow(dead_code, unused_variables, clippy::type_complexity)]
async fn using_function_pointer_interceptro() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await?;

    let client: GreeterClient<
        InterceptedService<Channel, fn(tonic::Request<()>) -> Result<tonic::Request<()>, Status>>,
    > = GreeterClient::with_interceptor(channel, intercept);

    Ok(())
}
