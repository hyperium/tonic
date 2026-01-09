//! Example demonstrating xds-client usage.
//!
//! This example shows:
//! - How to implement the `Resource` trait for Envoy Listener
//! - How to create an `XdsClient` with tonic transport and prost codec
//! - How to watch for resources and handle events
//!
//! # Usage
//!
//! Update `XDS_SERVER_URI` to point to your xDS management server.
//!
//! ```sh
//! cargo run -p xds-client --example basic
//! ```

use std::time::Duration;

use bytes::Bytes;
use prost::Message;

use xds_client::resource::TypeUrl;
use xds_client::{
    ClientConfig, ProstCodec, Resource, ResourceEvent, TokioRuntime, TonicTransport, XdsClient,
};

// Configuration - Update these values for your environment

/// URI of your xDS management server.
const XDS_SERVER_URI: &str = "http://localhost:18000";

/// Resource name to watch (or empty string "" for wildcard subscription).
const LISTENER_NAME: &str = "listener-1";

/// A simplified Listener resource.
///
/// In production, you might want to expose more fields from the proto.
#[derive(Debug, Clone)]
pub struct Listener {
    /// The listener name.
    pub name: String,
    /// The bind address.
    pub address: String,
    /// The bind port.
    pub port: u32,
}

impl Resource for Listener {
    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");

    fn decode(bytes: Bytes) -> xds_client::Result<Self> {
        use envoy_types::pb::envoy::config::core::v3::address::Address;
        use envoy_types::pb::envoy::config::core::v3::socket_address::PortSpecifier;
        use envoy_types::pb::envoy::config::listener::v3::Listener as ListenerProto;

        let proto = ListenerProto::decode(bytes)?;

        let (address, port) = proto
            .address
            .and_then(|addr| addr.address)
            .map(|addr| match addr {
                Address::SocketAddress(sa) => {
                    let port = match sa.port_specifier {
                        Some(PortSpecifier::PortValue(p)) => p,
                        _ => 0,
                    };
                    (sa.address, port)
                }
                _ => (String::new(), 0),
            })
            .unwrap_or_default();

        Ok(Self {
            name: proto.name,
            address,
            port,
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

    let config =
        ClientConfig::with_node_id("example-node").resource_timeout(Duration::from_secs(15));

    // For plaintext connection:
    let transport = TonicTransport::connect(XDS_SERVER_URI).await?;

    // For TLS, use tonic's Channel directly:
    //
    // use tonic::transport::{Certificate, Channel, ClientTlsConfig};

    // let ca_cert = std::fs::read_to_string("path/to/ca.pem")?;
    // let tls = ClientTlsConfig::new()
    //     .ca_certificate(Certificate::from_pem(&ca_cert))
    //     .domain_name("xds.example.com");

    // let channel = Channel::from_static("https://xds.example.com:443")
    //     .tls_config(tls)?
    //     .connect()
    //     .await?;

    // let transport = TonicTransport::from_channel(channel);

    println!("Connected!\n");

    let client = XdsClient::builder(config, transport, ProstCodec, TokioRuntime).build();

    println!("Watching for Listener: '{LISTENER_NAME}'");
    println!("(Use empty string for wildcard subscription)\n");

    let mut watcher = client.watch::<Listener>(LISTENER_NAME);

    // Event loop
    while let Some(event) = watcher.next().await {
        match event {
            ResourceEvent::ResourceChanged { resource, mut done } => {
                println!("✓ Listener received:");
                println!("  name:    {}", resource.name());
                println!("  address: {}:{}", resource.address, resource.port);
                println!();

                // In gRPC xDS, you would want to cascadingly subscribe to RDS, etc. before completing the done signal.
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

    println!("Watcher closed");
    Ok(())
}
