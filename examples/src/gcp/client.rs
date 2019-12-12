pub mod api {
    tonic::include_proto!("google.pubsub.v1");
}

use api::{publisher_client::PublisherClient, ListTopicsRequest};
use http::header::HeaderValue;
use tonic::{
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
        .skip(1)
        .next()
        .ok_or("Expected a project name as the first argument.".to_string())?;

    let bearer_token = format!("Bearer {}", token);
    let header_value = HeaderValue::from_str(&bearer_token)?;

    let certs = tokio::fs::read("tonic-examples/data/gcp/roots.pem").await?;

    let tls_config = ClientTlsConfig::with_rustls()
        .ca_certificate(Certificate::from_pem(certs.as_slice()))
        .domain_name("pubsub.googleapis.com");

    let channel = Channel::from_static(ENDPOINT)
        .intercept_headers(move |headers| {
            headers.insert("authorization", header_value.clone());
        })
        .tls_config(tls_config)
        .connect()
        .await?;

    let mut service = PublisherClient::new(channel);

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
