#![feature(async_await)]

use futures::Stream;
use std::time::Duration;
use tokio::timer::Delay;
use tonic::{Request, Response, Status};

// #[derive(Debug)]
// struct HelloRequest;
// #[derive(Debug)]
// struct HelloResponse;

#[derive(Default, Clone)]
pub struct MyGreeter {
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

    pub async fn server_stream(&self, request: Request<()>) -> Result<impl Stream, Status> {
        unimplemented!()
    }

    pub async fn client_stream(&self, request: Request<impl Stream>) -> Result<(), Status> {
        unimplemented!()
    }
}

#[tokio::test]
async fn grpc() {
    let svc = MyGreeter::default();
    let mut _server = GrpcServer::from(svc);
}
