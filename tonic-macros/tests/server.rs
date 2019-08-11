#![feature(async_await)]

use std::time::Duration;
use tokio::timer::Delay;
use tonic::{Request, Response, Status};

// #[derive(Debug)]
// struct HelloRequest;
// #[derive(Debug)]
// struct HelloResponse;

#[derive(Default, Clone)]
struct MyGreeter {
    data: String,
}

#[tonic::server(service = "proto/helloworld.proto")]
impl MyGreeter {
    pub async fn say_hello(&self, request: Request<()>) -> Result<Response<()>, Status> {
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
    let svc = MyGreeter::default();
    let mut server = GrpcServer::from(svc);

    use tower_service::Service;
    server.call(tonic::Request::new(())).await.unwrap();
}
