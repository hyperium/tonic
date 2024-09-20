#![allow(unused_imports)]

use self::util::*;
use crate::util::mock_io_channel;
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering::SeqCst},
        Arc,
    },
};
use tokio::net::TcpListener;
use tokio_stream::{Stream, StreamExt};
use tonic::{
    transport::{Channel, Endpoint, Server, Uri},
    Request, Response, Status, Streaming,
};
use tower::{layer::layer_fn, service_fn, Service, ServiceBuilder};
use tower_http::{map_request_body::MapRequestBodyLayer, map_response_body::MapResponseBodyLayer};

mod bidirectional_stream;
mod client_stream;
mod compressing_request;
mod compressing_response;
mod server_stream;
mod util;

tonic::include_proto!("test");

#[derive(Debug, Default)]
struct Svc {
    disable_compressing_on_response: bool,
}

const UNCOMPRESSED_MIN_BODY_SIZE: usize = 1024;

impl Svc {
    fn prepare_response<B>(&self, mut res: Response<B>) -> Response<B> {
        if self.disable_compressing_on_response {
            res.disable_compression();
        }

        res
    }
}

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn compress_output_unary(&self, _req: Request<()>) -> Result<Response<SomeData>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE];

        Ok(self.prepare_response(Response::new(SomeData {
            data: data.to_vec(),
        })))
    }

    async fn compress_input_unary(&self, req: Request<SomeData>) -> Result<Response<()>, Status> {
        assert_eq!(req.into_inner().data.len(), UNCOMPRESSED_MIN_BODY_SIZE);
        Ok(Response::new(()))
    }

    type CompressOutputServerStreamStream =
        Pin<Box<dyn Stream<Item = Result<SomeData, Status>> + Send + 'static>>;

    async fn compress_output_server_stream(
        &self,
        _req: Request<()>,
    ) -> Result<Response<Self::CompressOutputServerStreamStream>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
        let stream = tokio_stream::iter(std::iter::repeat(SomeData { data }))
            .take(2)
            .map(Ok::<_, Status>);
        Ok(self.prepare_response(Response::new(Box::pin(stream))))
    }

    async fn compress_input_client_stream(
        &self,
        req: Request<Streaming<SomeData>>,
    ) -> Result<Response<()>, Status> {
        let mut stream = req.into_inner();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }
        Ok(self.prepare_response(Response::new(())))
    }

    async fn compress_output_client_stream(
        &self,
        req: Request<Streaming<SomeData>>,
    ) -> Result<Response<SomeData>, Status> {
        let mut stream = req.into_inner();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }

        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE];

        Ok(self.prepare_response(Response::new(SomeData {
            data: data.to_vec(),
        })))
    }

    type CompressInputOutputBidirectionalStreamStream =
        Pin<Box<dyn Stream<Item = Result<SomeData, Status>> + Send + 'static>>;

    async fn compress_input_output_bidirectional_stream(
        &self,
        req: Request<Streaming<SomeData>>,
    ) -> Result<Response<Self::CompressInputOutputBidirectionalStreamStream>, Status> {
        let mut stream = req.into_inner();
        while let Some(item) = stream.next().await {
            item.unwrap();
        }

        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE].to_vec();
        let stream = tokio_stream::iter(std::iter::repeat(SomeData { data }))
            .take(2)
            .map(Ok::<_, Status>);
        Ok(self.prepare_response(Response::new(Box::pin(stream))))
    }
}
