#![allow(unused_imports)]

use std::sync::{
    atomic::{AtomicUsize, Ordering::Relaxed},
    Arc,
};
use tokio::net::TcpListener;
use tonic::{
    transport::{Channel, Server},
    Request, Response, Status,
};
use tower::{layer::layer_fn, Service, ServiceBuilder};
use tower_http::{map_request_body::MapRequestBodyLayer, map_response_body::MapResponseBodyLayer};

mod compressing_request;
mod compressing_response;
mod util;

tonic::include_proto!("test");

// TODO(david): client copmressing messages
// TODO(david): client streaming
// TODO(david): server streaming
// TODO(david): bidirectional streaming

struct Svc;

const UNCOMPRESSED_MIN_BODY_SIZE: usize = 1024;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn compress_output(&self, _req: Request<()>) -> Result<Response<SomeData>, Status> {
        let data = [0_u8; UNCOMPRESSED_MIN_BODY_SIZE];
        Ok(Response::new(SomeData {
            data: data.to_vec(),
        }))
    }

    async fn compress_input(&self, req: Request<SomeData>) -> Result<Response<()>, Status> {
        assert_eq!(req.into_inner().data.len(), UNCOMPRESSED_MIN_BODY_SIZE);
        Ok(Response::new(()))
    }
}
