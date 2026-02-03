//! Example demonstrating xds-client usage.
//!
//! This example shows:
//! - How to implement the `Resource` trait for Envoy Listener
//! - How to create an `XdsClient` with tonic transport and prost codec
//! - How to watch for resources and handle events
//!
//! # Usage
//!
//! ```sh
//! # Basic usage
//! cargo run -p xds-client --example basic -- -l my-listener
//!
//! # Multiple listeners
//! cargo run -p xds-client --example basic -- -l listener-1 -l listener-2
//!
//! # Custom server
//! cargo run -p xds-client --example basic -- -s http://xds.example.com:18000 -l foo
//!
//! # With TLS
//! cargo run -p xds-client --example basic -- \
//!   --ca-cert /path/to/ca.pem \
//!   --client-cert /path/to/client.pem \
//!   --client-key /path/to/client.key \
//!   -l my-listener
//! ```

use bytes::Bytes;
use clap::Parser;
use envoy_types::pb::envoy::config::listener::v3::Listener as ListenerProto;
use envoy_types::pb::envoy::extensions::filters::network::http_connection_manager::v3::{
    http_connection_manager::RouteSpecifier, HttpConnectionManager,
};
use prost::Message;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

use xds_client::resource::TypeUrl;
use xds_client::{
    ClientConfig, Node, ProstCodec, Resource, ResourceEvent, Result as XdsResult, ServerConfig,
    TokioRuntime, TonicTransport, TonicTransportBuilder, TransportBuilder, XdsClient,
};

/// Example demonstrating xds-client usage.
#[derive(Parser, Debug)]
#[command(name = "basic")]
#[command(about = "xds-client example - watch Listener resources")]
struct Args {
    /// URI of the xDS management server.
    #[arg(short, long, default_value = "http://localhost:18000")]
    server: String,

    /// Path to PEM-encoded CA certificate for TLS (enables TLS when set).
    #[arg(long)]
    ca_cert: Option<String>,

    /// Path to PEM-encoded client certificate for mTLS.
    #[arg(long, requires = "ca_cert")]
    client_cert: Option<String>,

    /// Path to PEM-encoded client key for mTLS.
    #[arg(long, requires = "client_cert")]
    client_key: Option<String>,

    /// Listener names to watch (pass multiple: -l foo -l bar).
    #[arg(short, long = "listener", required = true)]
    listeners: Vec<String>,
}

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

/// Custom transport builder that configures TLS on the channel.
///
/// This demonstrates how to implement a custom [`TransportBuilder`] when you need
/// TLS or other custom channel configuration. The default [`TonicTransportBuilder`]
/// creates plain (non-TLS) connections.
struct TlsTransportBuilder {
    tls_config: ClientTlsConfig,
}

impl TransportBuilder for TlsTransportBuilder {
    type Transport = TonicTransport;

    async fn build(&self, server: &ServerConfig) -> XdsResult<Self::Transport> {
        let channel = Channel::from_shared(server.uri.clone())
            .map_err(|e| xds_client::Error::Connection(e.to_string()))?
            .tls_config(self.tls_config.clone())
            .map_err(|e| xds_client::Error::Connection(e.to_string()))?
            .connect()
            .await
            .map_err(|e| xds_client::Error::Connection(e.to_string()))?;

        Ok(TonicTransport::from_channel(channel))
    }
}

impl Resource for Listener {
    type Message = ListenerProto;

    const TYPE_URL: TypeUrl = TypeUrl::new("type.googleapis.com/envoy.config.listener.v3.Listener");

    fn deserialize(bytes: Bytes) -> xds_client::Result<Self::Message> {
        ListenerProto::decode(bytes).map_err(Into::into)
    }

    fn name(message: &Self::Message) -> &str {
        &message.name
    }

    fn validate(message: Self::Message) -> xds_client::Result<Self> {
        let hcm = message
            .api_listener
            .and_then(|api| api.api_listener)
            .and_then(|any| HttpConnectionManager::decode(Bytes::from(any.value)).ok());

        let rds_route_config_name = hcm.and_then(|hcm| match hcm.route_specifier {
            Some(RouteSpecifier::Rds(rds)) => Some(rds.route_config_name),
            _ => None,
        });

        Ok(Self {
            name: message.name,
            rds_route_config_name,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.client_cert.is_some() != args.client_key.is_some() {
        eprintln!("Error: --client-cert and --client-key must be provided together");
        std::process::exit(1);
    }

    println!("xds-client Example\n");
    println!("Connecting to xDS server: {}", args.server);

    let node = Node::new("grpc", "1.0").with_id("example-node");
    let config = ClientConfig::new(node, &args.server);

    let client = match &args.ca_cert {
        Some(ca_path) => {
            let ca_cert = std::fs::read_to_string(ca_path)?;
            let mut tls = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&ca_cert));

            if let (Some(cert_path), Some(key_path)) = (&args.client_cert, &args.client_key) {
                let client_cert = std::fs::read_to_string(cert_path)?;
                let client_key = std::fs::read_to_string(key_path)?;
                tls = tls.identity(Identity::from_pem(client_cert, client_key));
            }

            let tls_builder = TlsTransportBuilder { tls_config: tls };
            XdsClient::builder(config, tls_builder, ProstCodec, TokioRuntime).build()
        }
        None => XdsClient::builder(
            config,
            TonicTransportBuilder::new(),
            ProstCodec,
            TokioRuntime,
        )
        .build(),
    };

    println!("Starting watchers...\n");

    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<ResourceEvent<Listener>>();

    // Start watchers for each listener from args
    for name in &args.listeners {
        println!("Watching for Listener: '{name}'");

        let mut watcher = client.watch::<Listener>(name);
        let tx = event_tx.clone();

        tokio::spawn(async move {
            while let Some(event) = watcher.next().await {
                if tx.send(event).is_err() {
                    eprintln!("Event channel closed, stopping watcher");
                    break;
                }
            }
        });
    }

    // Drop the original sender so the loop exits when all watchers complete
    drop(event_tx);

    while let Some(event) = event_rx.recv().await {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(resource),
                done,
            } => {
                println!("Listener received:");
                println!("  name:        {}", resource.name);
                if let Some(ref rds) = resource.rds_route_config_name {
                    println!("  rds_config:  {rds}");
                }
                println!();

                // In gRPC xDS, you would cascadingly subscribe to RDS, CDS, EDS, etc.
                // The done signal is sent automatically when it's dropped.
                drop(done);
            }

            ResourceEvent::ResourceChanged {
                result: Err(error), ..
            } => {
                // Resource was invalidated (validation error, deleted, etc.)
                println!("Resource invalidated: {error}");
            }

            ResourceEvent::AmbientError { error, .. } => {
                // Non-fatal error, continue using cached resource if available
                println!("Ambient error: {error}");
            }
        }
    }

    println!("Exiting");
    Ok(())
}
