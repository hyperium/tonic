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

use crate::attributes::Attributes;
use crate::credentials::common::Authority;
use crate::credentials::common::SecurityLevel;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

#[trait_variant::make(Send)]
pub trait ChannelCredsInternal {
    type ContextType: ClientConnectionSecurityContext;
    type Output<I>;
    /// Performs the client-side authentication handshake on a raw endpoint.
    ///
    /// This method wraps the provided `source` endpoint with the security protocol
    /// (e.g., TLS) and returns the authenticated endpoint along with its
    /// security details.
    ///
    /// # Arguments
    ///
    /// * `authority` - The `:authority` header value to be used when creating
    ///   new streams.
    ///   **Important:** Implementations must use this value as the server name
    ///   (e.g., for SNI) during the handshake.
    /// * `source` - The raw connection handle.
    /// * `info` - Additional context passed from the resolver or load balancer.
    async fn connect<Input: GrpcEndpoint>(
        &self,
        authority: &Authority,
        source: Input,
        info: ClientHandshakeInfo,
        runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<Self::Output<Input>, Self::ContextType>, String>;
}

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

    pub fn into_boxed(
        self,
    ) -> ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>
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
#[derive(Default)]
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
