#![allow(unused_variables, dead_code)]

use http_body::Body;
use integration_tests::pb::{test_server, Input, Output};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tonic::{transport::Server, Request, Response, Status};
use tower::{layer::Layer, BoxError, Service};

// all we care about is that this compiles
async fn complex_tower_layers_work() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, req: Request<Input>) -> Result<Response<Output>, Status> {
            unimplemented!()
        }
    }

    let svc = test_server::TestServer::new(Svc);

    Server::builder()
        .layer(MyServiceLayer::new())
        .add_service(svc)
        .serve("127.0.0.1:1322".parse().unwrap())
        .await
        .unwrap();
}

#[derive(Debug, Clone)]
struct MyServiceLayer {}

impl MyServiceLayer {
    fn new() -> Self {
        unimplemented!()
    }
}

impl<S> Layer<S> for MyServiceLayer {
    type Service = MyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        unimplemented!()
    }
}

#[derive(Debug, Clone)]
struct MyService<S> {
    inner: S,
}

impl<S, R, ResBody> Service<R> for MyService<S>
where
    S: Service<R, Response = http::Response<ResBody>>,
{
    type Response = http::Response<MyBody<ResBody>>;
    type Error = BoxError;
    type Future = MyFuture<S::Future, ResBody>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        unimplemented!()
    }

    fn call(&mut self, req: R) -> Self::Future {
        unimplemented!()
    }
}

struct MyFuture<F, B> {
    inner: F,
    body: B,
}

impl<F, E, B> Future for MyFuture<F, B>
where
    F: Future<Output = Result<http::Response<B>, E>>,
{
    type Output = Result<http::Response<MyBody<B>>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unimplemented!()
    }
}

struct MyBody<B> {
    inner: B,
}

impl<B> Body for MyBody<B>
where
    B: Body,
{
    type Data = B::Data;
    type Error = BoxError;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        unimplemented!()
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        unimplemented!()
    }
}
