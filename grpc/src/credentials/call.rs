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

use std::fmt::Debug;
use std::sync::Arc;

use tonic::Status;
use tonic::async_trait;
use tonic::metadata::MetadataMap;

use crate::attributes::Attributes;
use crate::credentials::common::SecurityLevel;

/// Details regarding the RPC call.
///
/// The fully qualified method name is constructed as:
/// `service_url` + "/" + `method_name`
pub struct CallDetails {
    service_url: String,
    method_name: String,
}

impl CallDetails {
    pub(crate) fn new(service_url: String, method_name: String) -> Self {
        Self {
            service_url,
            method_name,
        }
    }

    /// Returns the base URL of the service for this RPC call.
    pub fn service_url(&self) -> &str {
        &self.service_url
    }

    /// The method name suffix (e.g., `Method` or `package.Service/Method`).
    pub fn method_name(&self) -> &str {
        &self.method_name
    }
}

pub struct ChannelSecurityInfo {
    security_protocol: &'static str,
    security_level: SecurityLevel,
    /// Stores extra data derived from the underlying protocol.
    attributes: Attributes,
}

impl ChannelSecurityInfo {
    pub(crate) fn new(
        security_protocol: &'static str,
        security_level: SecurityLevel,
        attributes: Attributes,
    ) -> Self {
        Self {
            security_protocol,
            security_level,
            attributes,
        }
    }

    pub fn security_protocol(&self) -> &'static str {
        self.security_protocol
    }

    pub fn security_level(&self) -> SecurityLevel {
        self.security_level
    }

    pub fn attributes(&self) -> &Attributes {
        &self.attributes
    }
}

/// A thread-safe handle for credentials that are attached to individual RPC
/// calls.
///
/// `CallCredentials` wrap a [`CallCredentialsProvider`]. They can be combined
/// using [`CompositeCallCredentials`].
#[derive(Debug, Clone)]
pub struct CallCredentials {
    inner: Arc<dyn CallCredentialsProvider>,
}

impl CallCredentials {
    pub(crate) async fn get_metadata(
        &self,
        call_details: &CallDetails,
        auth_info: &ChannelSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), Status> {
        self.inner
            .get_metadata(call_details, auth_info, metadata)
            .await
    }

    pub(crate) fn minimum_channel_security_level(&self) -> SecurityLevel {
        self.inner.minimum_channel_security_level()
    }
}

impl<T: CallCredentialsProvider + 'static> From<T> for CallCredentials {
    fn from(provider: T) -> Self {
        Self {
            inner: Arc::new(provider),
        }
    }
}

/// Defines the interface for credentials that need to attach security
/// information to every individual RPC (e.g., OAuth2 tokens, JWTs).
#[async_trait]
pub trait CallCredentialsProvider: Send + Sync + Debug {
    /// Generates the authentication metadata for a specific RPC call.
    ///
    /// This method is called by the transport layer on each request.
    /// Implementations should populate the provided `metadata` map with the
    /// necessary authorization headers (e.g., `authorization: Bearer <token>`).
    ///
    /// If this returns an `Err`, the RPC will fail immediately with a status
    /// derived from the error if the status code is in the range defined in
    /// gRFC A54. Otherwise, the RPC is failed with an internal status.
    async fn get_metadata(
        &self,
        call_details: &CallDetails,
        auth_info: &ChannelSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), Status>;

    /// Indicates the minimum transport security level required to send
    /// these credentials.
    fn minimum_channel_security_level(&self) -> SecurityLevel {
        SecurityLevel::PrivacyAndIntegrity
    }
}

/// A composite implementation of [`CallCredentialsProvider`] that combines
/// multiple credentials.
#[derive(Debug)]
pub struct CompositeCallCredentials {
    creds: Vec<CallCredentials>,
}

impl CompositeCallCredentials {
    /// Creates a new [`CompositeCallCredentials`] with the first two credentials.
    pub fn new(first: impl Into<CallCredentials>, second: impl Into<CallCredentials>) -> Self {
        Self {
            creds: vec![first.into(), second.into()],
        }
    }

    /// Adds an additional [`CallCredentials`] to the composite.
    pub fn with_call_credentials(mut self, creds: impl Into<CallCredentials>) -> Self {
        self.creds.push(creds.into());
        self
    }
}

#[async_trait]
impl CallCredentialsProvider for CompositeCallCredentials {
    async fn get_metadata(
        &self,
        call_details: &CallDetails,
        auth_info: &ChannelSecurityInfo,
        metadata: &mut MetadataMap,
    ) -> Result<(), Status> {
        for cred in &self.creds {
            cred.get_metadata(call_details, auth_info, metadata).await?;
        }
        Ok(())
    }

    fn minimum_channel_security_level(&self) -> SecurityLevel {
        self.creds
            .iter()
            .map(|c| c.minimum_channel_security_level())
            .max_by_key(|&l| match l {
                SecurityLevel::NoSecurity => 0,
                SecurityLevel::IntegrityOnly => 1,
                SecurityLevel::PrivacyAndIntegrity => 2,
            })
            .unwrap_or(SecurityLevel::NoSecurity)
    }
}

#[cfg(test)]
mod tests {
    use tonic::metadata::MetadataValue;

    use super::*;

    #[derive(Debug)]
    struct MockCallCredentials {
        key: String,
        value: String,
        security_level: SecurityLevel,
    }

    #[async_trait]
    impl CallCredentialsProvider for MockCallCredentials {
        async fn get_metadata(
            &self,
            _call_details: &CallDetails,
            _auth_info: &ChannelSecurityInfo,
            metadata: &mut MetadataMap,
        ) -> Result<(), Status> {
            metadata.insert(
                self.key
                    .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                    .unwrap(),
                MetadataValue::try_from(&self.value).unwrap(),
            );
            Ok(())
        }

        fn minimum_channel_security_level(&self) -> SecurityLevel {
            self.security_level
        }
    }

    #[tokio::test]
    async fn test_composite_call_credentials() {
        let cred1 = MockCallCredentials {
            key: "key1".to_string(),
            value: "value1".to_string(),
            security_level: SecurityLevel::IntegrityOnly,
        };
        let cred2 = MockCallCredentials {
            key: "key2".to_string(),
            value: "value2".to_string(),
            security_level: SecurityLevel::PrivacyAndIntegrity,
        };

        let composite = CompositeCallCredentials::new(cred1, cred2);

        let call_details = CallDetails {
            service_url: "url".to_string(),
            method_name: "method".to_string(),
        };
        let auth_info = ChannelSecurityInfo::new(
            "test",
            SecurityLevel::PrivacyAndIntegrity,
            Attributes::new(),
        );
        let mut metadata = MetadataMap::new();

        composite
            .get_metadata(&call_details, &auth_info, &mut metadata)
            .await
            .unwrap();

        assert_eq!(metadata.get("key1").unwrap(), "value1");
        assert_eq!(metadata.get("key2").unwrap(), "value2");
        assert_eq!(
            composite.minimum_channel_security_level(),
            SecurityLevel::PrivacyAndIntegrity
        );
    }
}
