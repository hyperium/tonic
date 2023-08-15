#![allow(unused_imports)]

mod test_defaults;

use std::pin::Pin;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

tonic::include_proto!("test");
tonic::include_proto!("test_default");

#[derive(Debug, Default)]
struct Svc;

#[tonic::async_trait]
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

#[tonic::async_trait]
impl test_default_server::TestDefault for Svc {
    // Default unimplemented stubs provided here.
}
