/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

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
//! GRPC_XDS_BOOTSTRAP_CONFIG='{"xds_servers":[{"server_uri":"http://localhost:18000","channel_creds":[{"type":"insecure"}]}],"node":{"id":"test"}}' \
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

    let mut i = 1_u64;
    loop {
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
        i = i.checked_add(1).ok_or("request counter exhausted")?;
    }
}
