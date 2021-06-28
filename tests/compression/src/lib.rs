#![allow(unused_imports)]

use self::util::*;
use futures::{Stream, StreamExt};
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering::Relaxed},
        Arc,
    },
};
use tokio::net::TcpListener;
use tonic::{
    transport::{Channel, Server},
    Request, Response, Status, Streaming,
};
use tower::{layer::layer_fn, Service, ServiceBuilder};
use tower_http::{map_request_body::MapRequestBodyLayer, map_response_body::MapResponseBodyLayer};

mod client_stream;
mod compressing_request;
mod compressing_response;
mod server_stream;
mod util;

tonic::include_proto!("test");

// TODO(david): bidirectional streaming

struct Svc;

const UNCOMPRESSED_MIN_BODY_SIZE: usize = 1024;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn compress_output_unary(&self, _req: Request<()>) -> Result<Response<SomeData>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE];
        Ok(Response::new(SomeData {
            data: data.to_vec(),
        }))
    }

    async fn compress_input_unary(&self, req: Request<SomeData>) -> Result<Response<()>, Status> {
        assert_eq!(req.into_inner().data.len(), UNCOMPRESSED_MIN_BODY_SIZE);
        Ok(Response::new(()))
    }

    type CompressOutputServerStreamStream =
        Pin<Box<dyn Stream<Item = Result<SomeData, Status>> + Send + Sync + 'static>>;

    async fn compress_output_server_stream(
        &self,
        _req: Request<()>,
    ) -> Result<Response<Self::CompressOutputServerStreamStream>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
        let stream = futures::stream::repeat(SomeData { data }).map(Ok::<_, Status>);
        Ok(Response::new(Box::pin(stream)))
    }

    async fn compress_input_client_stream(
        &self,
        req: Request<Streaming<SomeData>>,
    ) -> Result<Response<()>, Status> {
        let mut stream = req.into_inner();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }
        Ok(Response::new(()))
    }
}
