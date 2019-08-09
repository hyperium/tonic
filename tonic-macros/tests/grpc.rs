#![feature(async_await)]

use tonic::{Request, Response, Status};
use tonic_macros::grpc;
use tokio::timer::Delay;
use std::time::Duration;

// #[derive(Debug)]
// struct HelloRequest;
// #[derive(Debug)]
// struct HelloResponse;

#[derive(Default, Clone)]
struct MyGreeter {
    data: String,
}

#[grpc(service = "proto/helloworld.proto")]
impl MyGreeter {
    pub async fn say_hello(&mut self, request: Request<()>) -> Result<Response<()>, Status> {
        println!("Got a request: {:?}", request);

        let string = &self.data;

        let when = tokio::clock::now() + Duration::from_millis(100);
        Delay::new(when).await;

        println!("My data: {:?}", string);

        Delay::new(when).await;

        Ok(Response::new(()))
    }
}

#[tokio::test]
async fn grpc() {
    let mut greeter = MyGreeter { data: "some data".into()};

    use tonic::GrpcInnerService;
    greeter.call(Request::new(())).await.unwrap();
}
