pub mod api {
    tonic::include_proto!("google.pubsub.v1");
}

use api::{publisher_client::PublisherClient, ListTopicsRequest};
use tonic::{
    metadata::MetadataValue,
    transport::{Certificate, Channel, ClientTlsConfig},
    Request,
};

const ENDPOINT: &str = "https://pubsub.googleapis.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("GCP_AUTH_TOKEN").map_err(|_| {
        "Pass a valid 0Auth bearer token via `GCP_AUTH_TOKEN` environment variable.".to_string()
    })?;

    let project = std::env::args()
        .nth(1)
        .ok_or_else(|| "Expected a project name as the first argument.".to_string())?;

    let bearer_token = format!("Bearer {}", token);
    let header_value: MetadataValue<_> = bearer_token.parse()?;

    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let certs = std::fs::read_to_string(data_dir.join("gcp/roots.pem"))?;

    let tls_config = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(certs))
        .domain_name("pubsub.googleapis.com");

    let channel = Channel::from_static(ENDPOINT)
        .tls_config(tls_config)?
        .connect()
        .await?;

    let mut service = PublisherClient::with_interceptor(channel, move |mut req: Request<()>| {
        req.metadata_mut()
            .insert("authorization", header_value.clone());
        Ok(req)
    });

    let response = service
        .list_topics(Request::new(ListTopicsRequest {
            project: format!("projects/{0}", project),
            page_size: 10,
            ..Default::default()
        }))
        .await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
