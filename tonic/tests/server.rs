#![feature(async_await, type_alias_impl_trait)]

use std::future::Future;
use std::task::{Context, Poll};
use tokio_buf::BufStream;
use tonic::codec::ProstCodec;
use tonic::server::*;
use tonic::{Request, Response, Status};
use std::pin::Pin;
use futures_core::Stream;

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

    fn call(&mut self, _request: Request<HelloRequest>) -> Self::Future {
        async move { Ok(Response::new(HelloReply { message: "hello".into()})) }
    }
}

struct SayHelloStream;

impl<S> ClientStreamingService<S> for SayHelloStream 
where S: Stream<Item = Result<HelloRequest, Status>> + Unpin + Send + 'static {
    type Response = HelloReply;
    type Future = impl Future<Output = Result<Response<Self::Response>, Status>>;

    fn call(&mut self, _: Request<S>) -> Self::Future {
        async move { Ok(Response::new(HelloReply { message: "hello".into()})) }
    }
}

#[tokio::test]
async fn say_hello() {
    let codec = ProstCodec::new();
    let mut grpc = Grpc::new(codec);

    let request = http::Request::new(Body(Vec::new()));
    grpc.unary(SayHello, request).await;

    let request = http::Request::new(Body(Vec::new()));
    grpc.client_streaming(SayHelloStream, request).await;
}

#[derive(Debug, Default, Clone)]
struct Body(Vec<u8>);

impl From<Vec<u8>> for Body {
    fn from(t: Vec<u8>) -> Self {
        Body(t)
    }
}

impl BufStream for Body {
    type Item = std::io::Cursor<Vec<u8>>;
    type Error = std::io::Error;

    fn poll_buf(&mut self, _cx: &mut Context<'_>) -> Poll<Option<Result<Self::Item, Self::Error>>> {
        if self.0.is_empty() {
            return None.into();
        }

        use std::{io, mem};

        let bytes = mem::replace(&mut self.0, Default::default());
        let buf = io::Cursor::new(bytes);

        Some(Ok(buf)).into()
    }
}
