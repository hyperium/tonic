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

//! Call an xDS-fronted service through GCP Traffic Director with manually
//! supplied Application Default Credentials.
//!
//! Supplies the Application Default Credentials (ADC) token as xDS call
//! credentials by implementing `TonicCallCredentials` directly against
//! `google-cloud-auth`.
//!
//! tonic-xds does not implement the bootstrap `google_default` credential
//! automatically. The bootstrap must select `tls`, while this example supplies
//! the ADC bearer token as call credentials:
//!
//! ```sh
//! GRPC_XDS_BOOTSTRAP=/path/to/bootstrap.json \
//!     cargo run -p tonic-xds --example channel_with_google_default --features "testutil tls-ring"
//! ```

use std::sync::Arc;

use google_cloud_auth::credentials::{AccessTokenCredentials, Builder};
use tonic_xds::testutil::proto::helloworld::{HelloRequest, greeter_client::GreeterClient};
use tonic_xds::{TonicCallCredentials, XdsChannelBuilder, XdsChannelConfig, XdsUri};

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Fetches ADC tokens directly from `google-cloud-auth`.
#[derive(Debug)]
struct AdcTonicCallCredentials {
    creds: AccessTokenCredentials,
}

impl AdcTonicCallCredentials {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let creds = Builder::default()
            .with_scopes([CLOUD_PLATFORM_SCOPE])
            .build_access_token_credentials()?;
        Ok(Self { creds })
    }
}

#[tonic::async_trait]
impl TonicCallCredentials for AdcTonicCallCredentials {
    async fn get_request_metadata(
        &self,
        metadata: &mut tonic::metadata::MetadataMap,
    ) -> std::result::Result<(), tonic::Status> {
        let token = self
            .creds
            .access_token()
            .await
            .map_err(|e| tonic::Status::unauthenticated(e.to_string()))?;
        let mut value =
            tonic::metadata::AsciiMetadataValue::try_from(format!("Bearer {}", token.token))
                .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;
        // Mark the bearer token sensitive so it is not accidentally logged.
        value.set_sensitive(true);
        metadata.insert("authorization", value);
        Ok(())
    }

    fn requires_secure_transport(&self) -> bool {
        true
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let target_str = std::env::var("XDS_TARGET").unwrap_or_else(|_| "xds:///my-service".into());
    let target = XdsUri::parse(&target_str)?;

    let creds: Arc<dyn TonicCallCredentials> = Arc::new(AdcTonicCallCredentials::new()?);

    let channel =
        XdsChannelBuilder::new(XdsChannelConfig::new(target).with_call_credentials(creds))
            .build_grpc_channel()?;

    let mut client = GreeterClient::new(channel);
    let response = client
        .say_hello(HelloRequest {
            name: "xds-gcp".into(),
        })
        .await?;

    println!("RESPONSE = {}", response.into_inner().message);
    Ok(())
}
