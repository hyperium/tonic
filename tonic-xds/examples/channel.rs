//! Example: send gRPC requests through an xDS-aware channel.
//!
//! Builds an xDS channel, then sends HelloRequest RPCs through it in a loop.
//! The channel discovers endpoints via the xDS management server and
//! load-balances across them.
//!
//! # Quick start
//!
//! Run all three examples (greeter backend, xDS server, this client) together:
//!
//! ```sh
//! ./tonic-xds/examples/run_xds_example.sh
//! ```
//!
//! # Running individually
//!
//! ```sh
//! # Terminal 1: greeter backend
//! PORT=50051 cargo run -p tonic-xds --example greeter_server --features testutil
//!
//! # Terminal 2: xDS control plane
//! cargo run -p tonic-xds --example xds_server
//!
//! # Terminal 3: xDS client
//! GRPC_XDS_BOOTSTRAP_CONFIG='{"xds_servers":[{"server_uri":"http://localhost:18000"}],"node":{"id":"test"}}' \
//!     cargo run -p tonic-xds --example channel --features testutil
//! ```
//!
//! # Configuration
//!
//! - `GRPC_XDS_BOOTSTRAP` — path to a bootstrap JSON file, **or**
//! - `GRPC_XDS_BOOTSTRAP_CONFIG` — inline bootstrap JSON
//! - `XDS_TARGET` — xDS target URI (default: `xds:///my-service`)

use tonic_xds::testutil::proto::helloworld::{HelloRequest, greeter_client::GreeterClient};
use tonic_xds::{XdsChannelBuilder, XdsChannelConfig, XdsUri};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target_str = std::env::var("XDS_TARGET").unwrap_or_else(|_| "xds:///my-service".into());
    let target = XdsUri::parse(&target_str)?;

    println!("Building xDS channel for target: {target_str}");

    let channel = XdsChannelBuilder::new(XdsChannelConfig::new(target)).build_grpc_channel()?;

    let mut client = GreeterClient::new(channel);

    println!("Channel built. Sending requests (Ctrl-C to stop)...\n");

    for i in 1.. {
        let request = HelloRequest {
            name: format!("request-{i}"),
        };

        match client.say_hello(request).await {
            Ok(response) => {
                println!("[{i}] Response: {}", response.into_inner().message);
            }
            Err(status) => {
                eprintln!("[{i}] Error: {status}");
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    Ok(())
}
