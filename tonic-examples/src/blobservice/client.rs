use env_logger;
use futures::TryStreamExt;
use log::{debug, error};

pub mod blobservice {
    tonic::include_proto!("blobservice");
}

use blobservice::{client::BlobberClient, BlobRequest};

#[tokio::main(single_thread)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut client = BlobberClient::connect("http://[::1]:50051")?;
    let nbytes = std::env::args().nth(1).unwrap().parse::<u64>().unwrap();
    let request = tonic::Request::new(BlobRequest { nbytes });

    let response = client.get_bytes(request).await?;
    let mut inner = response.into_inner();
    let mut i = 0_u64;
    while let Some(_) = inner.try_next().await.map_err(|e| {
        error!("i={}", i);
        error!("message={}", e.message());
        e
    })? {
        if i % 1000 == 0 {
            debug!("request # {}", i);
        }
        i += 1;
    }
    Ok(())
}
