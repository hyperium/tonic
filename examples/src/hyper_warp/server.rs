//! To hit the gRPC endpoint you must run this client via:
//! `cargo run --bin hyper-warp-client
//! To hit the warp server you can run this command:
//! `curl localhost:50051/hello`

use futures::future::{self, Either, TryFutureExt};
use http::version::Version;
use hyper::{service::make_service_fn, Server};
use pin_project::{pin_project, project};
use std::convert::Infallible;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tonic::{Request, Response, Status};
use tower::Service;
use warp::Filter;

use hello_world::greeter_server::{Greeter, GreeterServer};
use hello_world::{HelloReply, HelloRequest};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        let reply = hello_world::HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse().unwrap();
    let greeter = MyGreeter::default();

    println!("GreeterServer listening on {}", addr);

    let tonic = GreeterServer::new(greeter);
    let warp = warp::service(warp::path("hello").map(|| "hello, world!"));

    Server::bind(&addr)
        .serve(make_service_fn(move |_| {
            let mut tonic = tonic.clone();
            let mut warp = warp.clone();
            future::ok::<_, Infallible>(tower::service_fn(
                move |req: hyper::Request<hyper::Body>| match req.version() {
                    Version::HTTP_11 | Version::HTTP_10 => Either::Left(
                        warp.call(req)
                            .map_ok(|res| res.map(EitherBody::Left))
                            .map_err(Error::from),
                    ),
                    Version::HTTP_2 => Either::Right(
                        tonic
                            .call(req)
                            .map_ok(|res| res.map(EitherBody::Right))
                            .map_err(Error::from),
                    ),
                    _ => unimplemented!(),
                },
            ))
        }))
        .await?;

    Ok(())
}

#[pin_project]
enum EitherBody<A, B> {
    Left(#[pin] A),
    Right(#[pin] B),
}

impl<A, B> http_body::Body for EitherBody<A, B>
where
    A: http_body::Body + Send,
    B: http_body::Body<Data = A::Data> + Send,
    A::Error: Into<Error>,
    B::Error: Into<Error>,
{
    type Data = A::Data;
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

    fn is_end_stream(&self) -> bool {
        match self {
            EitherBody::Left(b) => b.is_end_stream(),
            EitherBody::Right(b) => b.is_end_stream(),
        }
    }

    #[project]
    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        #[project]
        match self.project() {
            EitherBody::Left(b) => b.poll_data(cx).map(map_option_err),
            EitherBody::Right(b) => b.poll_data(cx).map(map_option_err),
        }
    }

    #[project]
    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        #[project]
        match self.project() {
            EitherBody::Left(b) => b.poll_trailers(cx).map_err(Into::into),
            EitherBody::Right(b) => b.poll_trailers(cx).map_err(Into::into),
        }
    }
}

fn map_option_err<T, U: Into<Error>>(err: Option<Result<T, U>>) -> Option<Result<T, Error>> {
    err.map(|e| e.map_err(Into::into))
}
