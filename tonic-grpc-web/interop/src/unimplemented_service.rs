use tonic::{Request, Response, Status};

use crate::pb::{self, unimplemented_service_server::UnimplementedService};

#[derive(Default)]
pub struct Unimplemented;

#[tonic::async_trait]
impl UnimplementedService for Unimplemented {
    async fn unimplemented_call(
        &self,
        _req: Request<pb::Empty>,
    ) -> Result<Response<pb::Empty>, Status> {
        Err(Status::unimplemented(""))
    }
}
