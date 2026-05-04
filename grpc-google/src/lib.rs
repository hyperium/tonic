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

//! Google Cloud Platform (GCP) Credentials implementation for gRPC.
//!
//! This crate provides a way to create gRPC channel credentials that
//! automatically fetch and attach GCP authentication tokens (e.g., OAuth2)
//! using Application Default Credentials (ADC).

use std::fmt::Debug;

use google_cloud_auth::credentials::AccessTokenCredentials;
use grpc::Status;
use grpc::StatusCode;
use grpc::credentials::SecurityLevel;
use grpc::credentials::call::CallCredentials;
use grpc::credentials::call::CallDetails;
use grpc::credentials::call::ClientConnectionSecurityInfo;
use tonic::async_trait;
use tonic::metadata::AsciiMetadataValue;
use tonic::metadata::MetadataMap;

const DEFAULT_CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// An abstraction for fetching authentication tokens.
#[trait_variant::make(Send)]
pub trait TokenProvider: Sync + Debug + 'static {
    /// Returns an authentication token.
    async fn get_token(&self) -> Result<String, String>;
}

impl TokenProvider for AccessTokenCredentials {
    async fn get_token(&self) -> Result<String, String> {
        let token = self.access_token().await.map_err(|e| e.to_string())?;
        Ok(token.token)
    }
}

impl GcpCallCredentials<AccessTokenCredentials> {
    /// Returns credentials according to the standard
    /// [Application Default Credentials (ADC)][ADC-link] strategy.
    ///
    /// [ADC-link]: https://cloud.google.com/docs/authentication/application-default-credentials
    pub fn new_application_default() -> Result<Self, String> {
        Self::new_application_default_with_scope([DEFAULT_CLOUD_PLATFORM_SCOPE])
    }

    /// Returns credentials according to the standard
    /// [Application Default Credentials (ADC)][ADC-link] strategy, with
    /// specified scopes.
    ///
    /// [ADC-link]: https://cloud.google.com/docs/authentication/application-default-credentials
    pub fn new_application_default_with_scope<I, S>(scopes: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let credentials = google_cloud_auth::credentials::Builder::default()
            .with_scopes(scopes)
            .build_access_token_credentials()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            provider: credentials,
        })
    }
}

/// A call credentials implementation that fetches OAuth2 access tokens for GCP.
#[derive(Debug)]
pub struct GcpCallCredentials<P> {
    provider: P,
}

#[async_trait]
impl<P: TokenProvider> CallCredentials for GcpCallCredentials<P> {
    async fn get_metadata(
        &self,
        _call_details: &CallDetails,
        _auth_info: &ClientConnectionSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), Status> {
        let token = self
            .provider
            .get_token()
            .await
            .map_err(|e| Status::new(StatusCode::Unavailable, e))?;
        let mut value: AsciiMetadataValue = format!("Bearer {}", token).parse().map_err(|e| {
            Status::new(
                StatusCode::Internal,
                format!("invalid values in authorization header value: {}", e),
            )
        })?;
        value.set_sensitive(true);
        metadata.append("authorization", value);
        Ok(())
    }

    fn minimum_channel_security_level(&self) -> SecurityLevel {
        SecurityLevel::PrivacyAndIntegrity
    }
}

#[cfg(test)]
mod tests {
    use grpc::attributes::Attributes;

    use super::*;

    #[derive(Debug)]
    struct MockTokenProvider {
        result: Result<String, String>,
    }

    impl TokenProvider for MockTokenProvider {
        async fn get_token(&self) -> Result<String, String> {
            self.result.clone()
        }
    }

    fn fake_args() -> (CallDetails, ClientConnectionSecurityInfo) {
        let call_details = CallDetails::new("https://test.com", "/package.Service/TestMethod");
        let auth_info = ClientConnectionSecurityInfo::new(
            "tls",
            SecurityLevel::PrivacyAndIntegrity,
            Attributes::new(),
        );
        (call_details, auth_info)
    }

    #[tokio::test]
    async fn success() {
        let creds = GcpCallCredentials {
            provider: MockTokenProvider {
                result: Ok("valid_token".into()),
            },
        };
        let (cd, auth_info) = fake_args();
        let mut metadata = MetadataMap::new();

        let res = creds.get_metadata(&cd, &auth_info, &mut metadata).await;
        assert!(res.is_ok());

        let auth_header = metadata.get("authorization").unwrap();
        assert_eq!(auth_header.to_str().unwrap(), "Bearer valid_token");
    }

    #[tokio::test]
    async fn token_generation_failure() {
        let creds = GcpCallCredentials {
            provider: MockTokenProvider {
                result: Err("generation failed".into()),
            },
        };
        let (cd, auth_info) = fake_args();
        let mut metadata = MetadataMap::new();

        let res = creds.get_metadata(&cd, &auth_info, &mut metadata).await;
        let status = res.unwrap_err();
        assert_eq!(status.code(), grpc::StatusCode::Unavailable);
    }

    #[tokio::test]
    async fn non_ascii_token_internal_error() {
        let creds = GcpCallCredentials {
            provider: MockTokenProvider {
                result: Ok("invalid character\n".into()),
            },
        };
        let (cd, auth_info) = fake_args();
        let mut metadata = MetadataMap::new();

        let res = creds.get_metadata(&cd, &auth_info, &mut metadata).await;
        let status = res.unwrap_err();
        assert_eq!(status.code(), grpc::StatusCode::Internal);
    }

    #[test]
    fn security_level() {
        let creds = GcpCallCredentials {
            provider: MockTokenProvider {
                result: Ok("".into()),
            },
        };
        assert_eq!(
            creds.minimum_channel_security_level(),
            SecurityLevel::PrivacyAndIntegrity
        );
    }
}
