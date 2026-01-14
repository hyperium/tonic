//! Example demonstrating xds-client usage.
//!
//! This example shows:
//! - How to implement the `Resource` trait for Envoy Listener
//! - How to create an `XdsClient` with tonic transport and prost codec
//! - How to watch for resources and handle events
//!
//! # Usage
//!
//! Update the constants below to point to your xDS management server,
//! then run:
//!
//! ```sh
//! cargo run -p xds-client --example basic
//! ```
//!
//! Enter listener names to watch, one per line. Press Ctrl+C to exit.

use bytes::Bytes;
use envoy_types::pb::envoy::config::listener::v3::Listener as ListenerProto;
use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::{
    http_connection_manager::RouteSpecifier, HttpConnectionManager,
};
use prost::Message;
use tokio::io::{AsyncBufReadExt, BufReader};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

use xds_client::resource::TypeUrl;
use xds_client::{
    ClientConfig, ProstCodec, Resource, ResourceEvent, TokioRuntime, TonicTransport, XdsClient,
};

/// URI of your xDS management server.
const XDS_SERVER_URI: &str = "http://localhost:18000";

// Optional: paths for mTLS (set to None for plaintext)
const CA_CERT_PATH: Option<&str> = None; // e.g., Some("/path/to/ca.pem")
const CLIENT_CERT_PATH: Option<&str> = None; // e.g., Some("/path/to/client.pem")
const CLIENT_KEY_PATH: Option<&str> = None; // e.g., Some("/path/to/client.key")

// =============================================================================

/// A simplified Listener resource for gRPC xDS.
///
/// Extracts the RDS route config name from the ApiListener's HttpConnectionManager.
#[derive(Debug, Clone)]
pub struct Listener {
    /// The listener name.
    pub name: String,
    /// The RDS route config name (from HttpConnectionManager).
    pub rds_route_config_name: Option<String>,
}

impl Resource for Listener {
    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");

    fn decode(bytes: Bytes) -> xds_client::Result<Self> {
        let proto = ListenerProto::decode(bytes)?;

        let hcm = proto
            .api_listener
            .and_then(|api| api.api_listener)
            .and_then(|any| HttpConnectionManager::decode(Bytes::from(any.value)).ok());

        let rds_route_config_name = hcm.and_then(|hcm| match hcm.route_specifier {
            Some(RouteSpecifier::Rds(rds)) => Some(rds.route_config_name),
            _ => None,
        });

        Ok(Self {
            name: proto.name,
            rds_route_config_name,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== xds-client Example ===\n");
    println!("Connecting to xDS server: {XDS_SERVER_URI}");

    let config = ClientConfig::with_node_id("example-node").user_agent("grpc");

    let transport = match CA_CERT_PATH {
        Some(ca_path) => {
            let ca_cert = std::fs::read_to_string(ca_path)?;
            let mut tls = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&ca_cert));

            if let (Some(cert_path), Some(key_path)) = (CLIENT_CERT_PATH, CLIENT_KEY_PATH) {
                let client_cert = std::fs::read_to_string(cert_path)?;
                let client_key = std::fs::read_to_string(key_path)?;
                tls = tls.identity(Identity::from_pem(client_cert, client_key));
            }

            let channel = Channel::from_static(XDS_SERVER_URI)
                .tls_config(tls)?
                .connect()
                .await?;

            TonicTransport::from_channel(channel)
        }
        None => TonicTransport::connect(XDS_SERVER_URI).await?,
    };

    println!("Connected!\n");

    let client = XdsClient::builder(config, transport, ProstCodec, TokioRuntime).build();

    println!("Enter listener names to watch (one per line, Ctrl+C to exit):");
    println!("(Use empty string for wildcard subscription)\n");

    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<ResourceEvent<Listener>>();

    let client_clone = client.clone();
    let event_tx_clone = event_tx.clone();
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let name = line.trim().to_string();
            if name.is_empty() {
                continue;
            }

            println!("→ Watching for Listener: '{name}'");

            let mut watcher = client_clone.watch::<Listener>(&name);
            let tx = event_tx_clone.clone();

            tokio::spawn(async move {
                while let Some(event) = watcher.next().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });
        }
    });

    while let Some(event) = event_rx.recv().await {
        match event {
            ResourceEvent::ResourceChanged { resource, mut done } => {
                println!("✓ Listener received:");
                println!("  name:        {}", resource.name());
                if let Some(ref rds) = resource.rds_route_config_name {
                    println!("  rds_config:  {rds}");
                }
                println!();

                // In gRPC xDS, you would cascadingly subscribe to RDS, CDS, EDS, etc.
                // before completing the done signal.
                done.complete();
            }

            ResourceEvent::ResourceError { error, .. } => {
                println!("✗ Resource error: {error}");
            }

            ResourceEvent::AmbientError { error, .. } => {
                println!("⚠ Connection error: {error}");
            }
        }
    }

    println!("Exiting");
    Ok(())
}
