use futures::TryStreamExt;

pub mod blobservice {
    tonic::include_proto!("blobservice");
}

use blobservice::{client::BlobberClient, BlobRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = BlobberClient::connect("http://[::1]:50051")?;
    let nbytes = std::env::args().nth(1).unwrap().parse::<u64>().unwrap();
    let request = tonic::Request::new(BlobRequest { nbytes });

    let response = client.get_bytes(request).await?;
    let mut inner = response.into_inner();
    let mut i = 0_u64;
    while let Some(_) = inner.try_next().await.map_err(|e| {
        println!("i={}", i);
        println!("message={}", e.message());
        e
    })? {
        i += 1;
    }
    Ok(())
}
