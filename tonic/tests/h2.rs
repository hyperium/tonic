#![feature(async_await, type_alias_impl_trait)]

use futures_util::future;
use futures_core::Stream;
use std::pin::Pin;
use std::future::Future;
use std::task::{Context, Poll};
use tokio::net::TcpListener;
use tonic::{
    body,
    server::{Grpc, UnaryService, ClientStreamingService},
    Request, Response, Status,
};
use tower_h2::{RecvBody, Server};
use tower_service::Service;

#[derive(Clone, PartialEq, prost::Message)]
pub struct HelloRequest {
    #[prost(string, tag = "1")]
    pub name: std::string::String,
}
/// The response message containing the greetings
#[derive(Clone, PartialEq, prost::Message)]
pub struct HelloReply {
    #[prost(string, tag = "1")]
    pub message: std::string::String,
}

struct SayHello;

impl UnaryService<HelloRequest> for SayHello {
    type Response = HelloReply;
    type Future = impl Future<Output = Result<Response<Self::Response>, Status>>;

    fn call(&mut self, request: Request<HelloRequest>) -> Self::Future {
        async move {
            println!("REQUEST = {:?}", request);

            let reply = HelloReply {
                message: "Zomg, it works!".to_string(),
            };

            Ok(Response::new(reply))
        }
    }
}

struct SayHelloStream;

impl<S> ClientStreamingService<S> for SayHelloStream 
where S: Stream<Item = Result<HelloRequest, Status>> + Unpin + Send + 'static {
    type Response = HelloReply;
    // type Future = impl Future<Output = Result<Response<Self::Response>, Status>>;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send + 'static>>;

    fn call(&mut self, _req: Request<S>) -> Self::Future {
        let fut = async move { 
            Ok(Response::new(HelloReply { message: "hello".into()})) 
        };
        Box::pin(fut)
    }
}

#[tokio::test]
async fn main() {
    let addr = "[::1]:50051".parse().unwrap();
    let mut bind = TcpListener::bind(&addr).unwrap();

    let mut server = Server::new(MakeSvc, Default::default());

    while let Ok((sock, _addr)) = bind.accept().await {
        if let Err(e) = sock.set_nodelay(true) {
            panic!("error: {}", e);
        }

        if let Err(e) = server.serve(sock).await {
            println!("H2 ERROR: {}", e);
        }
    }
}

#[derive(Debug)]
pub struct Svc;

impl Service<http::Request<RecvBody>> for Svc {
    type Response = http::Response<body::BoxAsyncBody>;
    type Error = tonic::error::Never;
    // type Future = impl Future<Output = Result<Self::Response, Self::Error>>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: http::Request<RecvBody>) -> Self::Future {
        match req.uri().path() {
            "/greeter.Helloworld/SayHello" => {
                let fut = async move {
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = Grpc::new(codec);
                    let response = grpc.unary(SayHello, req).await;
                    Ok(response)
                };

                Box::pin(fut)
            }

            "/greeter.Helloworld/SayHelloStreaming" => {
                let fut = async move {
                    let codec = tonic::codec::ProstCodec::new();
                    let mut grpc = Grpc::new(codec);
                    let response = grpc.client_streaming(SayHelloStream, req).await;
                    Ok(response)
                };

                Box::pin(fut)
            }

            _ => unimplemented!()
        }
        
    }
}

pub struct MakeSvc;

impl Service<()> for MakeSvc {
    type Response = Svc;
    type Error = std::io::Error;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, _: ()) -> Self::Future {
        future::ok(Svc)
    }
}
