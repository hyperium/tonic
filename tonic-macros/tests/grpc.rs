#![feature(async_await)]

use std::time::Duration;
use tokio::timer::Delay;
use tonic::{Request, Response, Status};
use tonic_macros::grpc;

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

use std::future::Future;
use std::task::{Poll, Context};
pub trait Service<'a, Request> {
    type Response;
    type Error;
    type Future: Future<Output = Result<Self::Response, Self::Error>> + 'a;

    fn poll_ready(&'a mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
    fn call(&'a mut self, req: Request) -> Self::Future;
}

struct Svc {
    inner: MyGreeter,
}

impl<'a> Service<'a, tonic::Request<()>> for Svc {
    type Response = tonic::Response<()>;
    type Error = tonic::Status;
    type Future = tonic::ResponseFuture<'a, Self::Response>;

    fn poll_ready(
        &'a mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&'a mut self, request: tonic::Request<()>) -> Self::Future {
        use tonic::GrpcInnerService;
        self.inner.call(request)
    }
}

#[tokio::test]
async fn grpc() {
    let greeter = MyGreeter {
        data: "some data".into(),
    };

    let mut svc = Svc { inner: greeter };

    svc.call(Request::new(())).await.unwrap();
}
