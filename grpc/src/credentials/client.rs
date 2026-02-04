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

use tonic::async_trait;

use crate::attributes::Attributes;
use crate::credentials::common::{Authority, SecurityLevel};
use crate::rt::{GrpcEndpoint, Runtime};

#[async_trait]
pub trait Sealed {
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
    async fn connect<Input: GrpcEndpoint + 'static>(
        &self,
        authority: &Authority,
        source: Input,
        info: ClientHandshakeInfo,
        runtime: Arc<dyn Runtime>,
    ) -> Result<
        (
            Self::Output<Input>,
            ClientConnectionSecurityInfo<Self::ContextType>,
        ),
        String,
    >;
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

/// Represents the security state of an established client-side connection.
#[non_exhaustive]
pub struct ClientConnectionSecurityInfo<C> {
    pub(crate) security_protocol: &'static str,
    pub(crate) security_level: SecurityLevel,
    pub(crate) security_context: C,
    /// Stores extra data derived from the underlying protocol.
    pub(crate) attributes: Attributes,
}

/// Holds data to be passed during the connection handshake.
///
/// This mechanism allows arbitrary data to flow from gRPC core components—such
/// as resolvers and load balancers—down to the credential implementations.
///
/// Individual credential implementations are responsible for validating and
/// interpreting the format of the data they receive.
#[derive(Default)]
#[non_exhaustive]
pub struct ClientHandshakeInfo {
    /// The bag of attributes containing the handshake data.
    pub attributes: Attributes,
}
