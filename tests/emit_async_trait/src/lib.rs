#![allow(unused_imports)]

use std::pin::Pin;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

tonic::include_proto!("test");

#[derive(Debug, Default)]
struct Svc;

#[cfg_attr(feature = "async_trait", tonic::async_trait)]
impl test_server::Test for Svc {
    type ServerStreamStream = Pin<Box<dyn Stream<Item = Result<(), Status>> + Send + 'static>>;
    type BidirectionalStreamStream =
        Pin<Box<dyn Stream<Item = Result<(), Status>> + Send + 'static>>;

    async fn unary(&self, _: Request<()>) -> Result<Response<()>, Status> {
        Err(Status::permission_denied(""))
    }

    async fn server_stream(
        &self,
        _: Request<()>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        Err(Status::permission_denied(""))
    }

    async fn client_stream(&self, _: Request<Streaming<()>>) -> Result<Response<()>, Status> {
        Err(Status::permission_denied(""))
    }

    async fn bidirectional_stream(
        &self,
        _: Request<Streaming<()>>,
    ) -> Result<Response<Self::BidirectionalStreamStream>, Status> {
        Err(Status::permission_denied(""))
    }
}
