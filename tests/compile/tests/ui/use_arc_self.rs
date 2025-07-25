use std::sync::Arc;
use tonic::{Request, Response, Status};

tonic::include_proto!("use_arc_self");

struct Svc;

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn test_request(
        self: Arc<Self>,
        req: Request<SomeData>,
    ) -> Result<Response<SomeData>, Status> {
        Ok(Response::new(req.into_inner()))
    }
}

fn main() {}
