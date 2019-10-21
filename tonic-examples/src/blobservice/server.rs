use futures::Stream;
use std::convert::TryInto;
use std::iter::FromIterator;
use std::pin::Pin;
use tonic::{transport::Server, Request, Response, Status};

mod blobservice {
    tonic::include_proto!("blobservice");
}

use blobservice::{
    server::{Blobber, BlobberServer},
    BlobRequest, BlobResponse,
};

#[derive(Default)]
struct SimpleBlobber;

#[tonic::async_trait]
impl Blobber for SimpleBlobber {
    type GetBytesStream =
        Pin<Box<dyn Stream<Item = Result<BlobResponse, Status>> + Send + 'static>>;

    async fn get_bytes(
        &self,
        request: Request<BlobRequest>,
    ) -> Result<Response<Self::GetBytesStream>, Status> {
        let message = request.into_inner();
        let bytes =
            Vec::from_iter(std::iter::repeat(254u8).take(message.nbytes.try_into().unwrap()));
        let response = futures::stream::iter((0..).map(move |_| {
            Ok(blobservice::BlobResponse {
                bytes: bytes.clone(),
            })
        }));

        Ok(Response::new(Box::pin(response) as Self::GetBytesStream))
    }
}

#[tokio::main(single_thread)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address = "[::1]:50051".parse().unwrap();
    Server::builder()
        .serve(address, BlobberServer::new(SimpleBlobber::default()))
        .await?;

    Ok(())
}
