#[allow(unused)]
pub mod api {
    grpc::include_proto!("google/pubsub/v1", "pubsub");
}

use std::sync::Arc;

use api::ListTopicsRequest;
use api::publisher_client::PublisherClient;
use grpc::client::Channel;
use grpc::credentials::CompositeChannelCredentials;
use grpc::credentials::rustls::client::ClientTlsConfig;
use grpc::credentials::rustls::client::RustlsChannelCredendials;
use grpc_google::GcpCallCredentials;
use protobuf::proto;

const ENDPOINT: &str = "dns:///pubsub.googleapis.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    _ = rustls::crypto::ring::default_provider().install_default();
    let project = std::env::args()
        .nth(1)
        .ok_or_else(|| "Expected a project name as the first argument.".to_string())?;

    let call_creds = GcpCallCredentials::new_application_default()?;
    let tls = RustlsChannelCredendials::new(ClientTlsConfig::new())?;
    let channel_creds = CompositeChannelCredentials::new(tls, Arc::new(call_creds))?;

    let channel = Channel::new(ENDPOINT, Arc::new(channel_creds), Default::default());

    let client = PublisherClient::new(channel);

    let response = client
        .list_topics(proto!(ListTopicsRequest {
            project: format!("projects/{project}"),
        }))
        .await;

    println!("RESPONSE={response:?}");

    Ok(())
}
