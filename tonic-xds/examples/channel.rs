//! Example: send gRPC requests through an xDS-aware channel.
//!
//! Builds an xDS channel, then sends HelloRequest RPCs through it in a loop.
//! The channel discovers endpoints via the xDS management server and
//! load-balances across them.
//!
//! # Prerequisites
//!
//! 1. Start one or more greeter backends:
//!    ```sh
//!    PORT=50051 cargo run -p tonic-xds --example greeter_server
//!    ```
//!
//! 2. Start an xDS control plane (e.g., go-control-plane) configured to
//!    return LDS/RDS/CDS/EDS pointing at the greeter backends.
//!
//! # Configuration
//!
//! - `GRPC_XDS_BOOTSTRAP` — path to a bootstrap JSON file, **or**
//! - `GRPC_XDS_BOOTSTRAP_CONFIG` — inline bootstrap JSON
//! - `XDS_TARGET` — xDS target URI (default: `xds:///my-service`)
//!
//! # Usage
//!
//! ```sh
//! GRPC_XDS_BOOTSTRAP_CONFIG='{"xds_servers":[{"server_uri":"localhost:18000"}],"node":{"id":"test"}}' \
//!     cargo run -p tonic-xds --example channel
//! ```

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
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

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
    }

    Ok(())
}
