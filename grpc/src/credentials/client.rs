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

use std::sync::Arc;

use crate::attributes::Attributes;
use crate::credentials::ChannelCredentials;
use crate::credentials::ProtocolInfo;
use crate::credentials::SecurityLevel;
use crate::credentials::call::CallCredentials;
use crate::credentials::call::CompositeCallCredentials;
use crate::credentials::common::Authority;
use crate::credentials::insecure;
use crate::private;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

pub struct HandshakeOutput<T, C: ClientConnectionSecurityContext> {
    pub endpoint: T,
    pub security: ClientConnectionSecurityInfo<C>,
}

pub trait ClientConnectionSecurityContext: Send + Sync + 'static {
    /// Checks if the established connection is authorized to send requests to
    /// the given authority.
    ///
    /// This is primarily used for HTTP/2 connection reuse (coalescing). If the
    /// underlying security handshake (e.g., a TLS certificate) covers the provided
    /// `authority`, the existing connection may be reused for that host.
    ///
    /// # Returns
    ///
    /// * `true` - The connection is valid for this authority.
    /// * `false` - The connection cannot be reused; a new connection must be created.
    fn validate_authority(&self, authority: &Authority) -> bool {
        false
    }
}

impl ClientConnectionSecurityContext for Box<dyn ClientConnectionSecurityContext> {
    fn validate_authority(&self, authority: &Authority) -> bool {
        (**self).validate_authority(authority)
    }
}

/// Represents the security state of an established client-side connection.
pub struct ClientConnectionSecurityInfo<C> {
    security_protocol: &'static str,
    security_level: SecurityLevel,
    security_context: C,
    /// Stores extra data derived from the underlying protocol.
    attributes: Attributes,
}

pub type DynClientConnectionSecurityInfo =
    ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>;

impl<C> ClientConnectionSecurityInfo<C> {
    pub fn new(
        security_protocol: &'static str,
        security_level: SecurityLevel,
        security_context: C,
        attributes: Attributes,
    ) -> Self {
        Self {
            security_protocol,
            security_level,
            security_context,
            attributes,
        }
    }

    pub fn security_protocol(&self) -> &'static str {
        self.security_protocol
    }

    pub fn security_level(&self) -> SecurityLevel {
        self.security_level
    }

    pub fn security_context(&self) -> &C {
        &self.security_context
    }

    pub fn attributes(&self) -> &Attributes {
        &self.attributes
    }

    pub fn into_boxed(self) -> DynClientConnectionSecurityInfo
    where
        C: ClientConnectionSecurityContext + 'static,
    {
        ClientConnectionSecurityInfo {
            security_protocol: self.security_protocol,
            security_level: self.security_level,
            security_context: Box::new(self.security_context),
            attributes: self.attributes,
        }
    }
}

/// Holds data to be passed during the connection handshake.
///
/// This mechanism allows arbitrary data to flow from gRPC core components—such
/// as resolvers and load balancers—down to the credential implementations.
///
/// Individual credential implementations are responsible for validating and
/// interpreting the format of the data they receive.
#[derive(Default, Clone)]
pub struct ClientHandshakeInfo {
    /// The bag of attributes containing the handshake data.
    attributes: Attributes,
}

impl ClientHandshakeInfo {
    pub fn new(attributes: Attributes) -> Self {
        Self { attributes }
    }

    pub fn attributes(&self) -> &Attributes {
        &self.attributes
    }
}

/// A credential that combines [`ChannelCredentials`] with [`CallCredentials`].
///
/// This is used to attach per-call authentication (like OAuth2 tokens) to a
/// secure channel (like TLS).
pub struct CompositeChannelCredentials<T> {
    channel_creds: T,
    call_creds: Arc<dyn CallCredentials>,
}

impl<T: ChannelCredentials> CompositeChannelCredentials<T> {
    pub fn new(channel_creds: T, call_creds: Arc<dyn CallCredentials>) -> Result<Self, String> {
        if channel_creds.info().security_protocol() == insecure::PROTOCOL_NAME {
            return Err("using tokens on an insecure credentials is disallowed".to_string());
        }

        let combined_call_creds =
            if let Some(existing) = channel_creds.get_call_credentials(private::Internal) {
                let composite_creds = CompositeCallCredentials::new(existing.clone(), call_creds);
                Arc::new(composite_creds)
            } else {
                call_creds
            };

        Ok(Self {
            channel_creds,
            call_creds: combined_call_creds,
        })
    }
}

impl<T: ChannelCredentials> ChannelCredentials for CompositeChannelCredentials<T> {
    type ContextType = T::ContextType;
    type Output<I> = T::Output<I>;

    async fn connect<Input: GrpcEndpoint>(
        &self,
        authority: &Authority,
        source: Input,
        info: &ClientHandshakeInfo,
        runtime: &GrpcRuntime,
        token: private::Internal,
    ) -> Result<HandshakeOutput<Self::Output<Input>, Self::ContextType>, String> {
        self.channel_creds
            .connect(authority, source, info, runtime, token)
            .await
    }

    fn info(&self) -> &ProtocolInfo {
        self.channel_creds.info()
    }

    fn get_call_credentials(&self, _: private::Internal) -> Option<&Arc<dyn CallCredentials>> {
        Some(&self.call_creds)
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tonic::async_trait;
    use tonic::metadata::MetadataMap;
    use tonic::metadata::MetadataValue;

    use super::*;
    use crate::StatusErr;
    use crate::credentials::call::CallCredentials;
    use crate::credentials::call::CallDetails;
    use crate::credentials::call::ClientConnectionSecurityInfo;
    use crate::credentials::insecure::InsecureChannelCredentials;
    use crate::credentials::local::LocalChannelCredentials;
    use crate::rt;
    use crate::rt::TcpOptions;

    #[derive(Debug)]
    struct MockCallCredentials {
        key: &'static str,
        value: &'static str,
        min_security_level: SecurityLevel,
    }

    #[async_trait]
    impl CallCredentials for MockCallCredentials {
        async fn get_metadata(
            &self,
            _call_details: &CallDetails,
            _auth_info: &ClientConnectionSecurityInfo,
            metadata: &mut MetadataMap,
        ) -> Result<(), StatusErr> {
            metadata.insert(
                self.key
                    .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                    .unwrap(),
                MetadataValue::try_from(self.value).unwrap(),
            );
            Ok(())
        }

        fn minimum_channel_security_level(&self) -> SecurityLevel {
            self.min_security_level
        }
    }

    #[tokio::test]
    async fn test_multiple_composition() {
        let channel_creds = LocalChannelCredentials::new();
        let call_creds1 = Arc::new(MockCallCredentials {
            key: "auth1",
            value: "val1",
            min_security_level: SecurityLevel::IntegrityOnly,
        });
        let call_creds2 = Arc::new(MockCallCredentials {
            key: "auth2",
            value: "val2",
            min_security_level: SecurityLevel::PrivacyAndIntegrity,
        });

        // First composition.
        let composite1 = CompositeChannelCredentials::new(channel_creds, call_creds1).unwrap();

        // Second composition (using the first composite as base).
        let composite2 = CompositeChannelCredentials::new(composite1, call_creds2).unwrap();

        // Verify call credentials
        let combined_call_creds = composite2.get_call_credentials(private::Internal).unwrap();
        let call_details = CallDetails::new("service".to_string(), "method".to_string());
        let auth_info = ClientConnectionSecurityInfo::new(
            "local",
            SecurityLevel::NoSecurity,
            Attributes::new(),
        );
        let mut metadata = MetadataMap::new();

        combined_call_creds
            .get_metadata(&call_details, &auth_info, &mut metadata)
            .await
            .unwrap();

        assert_eq!(metadata.get("auth1").unwrap(), "val1");
        assert_eq!(metadata.get("auth2").unwrap(), "val2");

        // Verify min security level is the max of both.
        assert_eq!(
            combined_call_creds.minimum_channel_security_level(),
            SecurityLevel::PrivacyAndIntegrity
        );

        // Verify security level
        let addr = "127.0.0.1:0";
        let listener = TcpListener::bind(addr).await.unwrap();
        let server_addr = listener.local_addr().unwrap();
        let authority = Authority::new("localhost".to_string(), Some(server_addr.port()));
        let runtime = rt::default_runtime();
        let endpoint = runtime
            .tcp_stream(server_addr, TcpOptions::default())
            .await
            .unwrap();

        let output = composite2
            .connect(
                &authority,
                endpoint,
                &ClientHandshakeInfo::default(),
                &runtime,
                private::Internal,
            )
            .await
            .unwrap();
        assert_eq!(output.security.security_level(), SecurityLevel::NoSecurity);
        assert_eq!(output.security.security_protocol(), "local");
    }

    #[test]
    fn test_composite_channel_credentials_insecure() {
        let channel_creds = InsecureChannelCredentials::new();
        let call_creds = Arc::new(MockCallCredentials {
            key: "auth",
            value: "val",
            min_security_level: SecurityLevel::NoSecurity,
        });
        let result = CompositeChannelCredentials::new(channel_creds, call_creds);
        assert!(result.is_err());
    }
}
