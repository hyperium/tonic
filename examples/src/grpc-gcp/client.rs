/*
 *
 * Copyright 2026 gRPC authors.
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

#[allow(unused, clippy::all)]
mod generated {
    pub mod api {
        grpc::include_proto!("google/pubsub/v1", "pubsub");
    }
}

use std::sync::Arc;

use generated::api::ListTopicsRequest;
use generated::api::publisher_client::PublisherClient;
use grpc::client::Channel;
use grpc::credentials::CompositeChannelCredentials;
use grpc_google_cloud_auth::GcpCallCredentials;
use grpc_rustls::client::ClientTlsConfig;
use grpc_rustls::client::RustlsChannelCredentials;
use protobuf::proto;

const ENDPOINT: &str = "dns:///pubsub.googleapis.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    _ = rustls::crypto::ring::default_provider().install_default();
    let project = std::env::args()
        .nth(1)
        .ok_or_else(|| "Expected a project name as the first argument.".to_string())?;

    let call_creds = GcpCallCredentials::new_application_default()?;
    let tls = RustlsChannelCredentials::new(ClientTlsConfig::new())?;
    let channel_creds = CompositeChannelCredentials::new(tls, Arc::new(call_creds));

    let channel = Channel::builder(ENDPOINT, Arc::new(channel_creds)).build();

    let client = PublisherClient::new(channel);

    let response = client
        .list_topics(proto!(ListTopicsRequest {
            project: format!("projects/{project}"),
        }))
        .await;

    println!("RESPONSE={response:?}");

    Ok(())
}
