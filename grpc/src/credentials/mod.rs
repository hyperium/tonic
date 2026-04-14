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

pub mod call;
pub(crate) mod client;
pub(crate) mod dyn_wrapper;
mod insecure;
mod local;
#[cfg(feature = "tls-rustls")]
pub mod rustls;
pub(crate) mod server;

use std::sync::Arc;

pub use client::CompositeChannelCredentials;
pub use insecure::InsecureChannelCredentials;
pub use insecure::InsecureServerCredentials;
pub use local::LocalChannelCredentials;
pub use local::LocalServerCredentials;

use crate::credentials::call::CallCredentials;
use crate::credentials::client::ClientConnectionSecurityContext;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::client::HandshakeOutput;
use crate::credentials::common::Authority;
use crate::private;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

/// Defines the common interface for all live gRPC wire protocols and supported
/// transport security protocols (e.g., TLS, ALTS).
#[trait_variant::make(Send)]
pub trait ChannelCredentials: Sync + 'static {
    #[doc(hidden)]
    type ContextType: ClientConnectionSecurityContext;
    #[doc(hidden)]
    type Output<I>;

    //// Provides the ProtocolInfo of these credentials.
    fn info(&self) -> &ProtocolInfo;

    /// Returns call credentials to be used for all RPCs made on a connection.
    #[doc(hidden)]
    fn get_call_credentials(&self, token: private::Internal) -> Option<&Arc<dyn CallCredentials>>;

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
    #[doc(hidden)]
    async fn connect<Input: GrpcEndpoint>(
        &self,
        authority: &Authority,
        source: Input,
        info: &ClientHandshakeInfo,
        runtime: &GrpcRuntime,
        token: private::Internal,
    ) -> Result<HandshakeOutput<Self::Output<Input>, Self::ContextType>, String>;
}

#[trait_variant::make(Send)]
pub trait ServerCredentials: Sync + 'static {
    #[doc(hidden)]
    type Output<I>;

    //// Provides the ProtocolInfo of this credentials.
    fn info(&self) -> &ProtocolInfo;

    /// Performs the server-side authentication handshake.
    ///
    /// This method wraps the incoming raw `source` connection with the configured
    /// security protocol (e.g., TLS).
    #[doc(hidden)]
    async fn accept<Input: GrpcEndpoint>(
        &self,
        source: Input,
        runtime: GrpcRuntime,
        token: private::Internal,
    ) -> Result<server::HandshakeOutput<Self::Output<Input>>, String>;
}

/// Defines the level of protection provided by an established connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SecurityLevel {
    /// The connection is insecure; no protection is applied.
    NoSecurity,
    /// The connection guarantees data integrity (tamper-proofing) but not
    /// privacy.
    ///
    /// Payloads are visible to observers but cannot be modified without
    /// detection.
    IntegrityOnly,
    /// The connection guarantees both privacy (confidentiality) and data
    /// integrity.
    ///
    /// This is the standard level for secure transports like TLS.
    PrivacyAndIntegrity,
}

pub(crate) mod common {
    /// Represents the value passed as the `:authority` pseudo-header, typically
    /// in the form `host:port`.
    #[derive(Clone, PartialEq, Debug)]
    pub struct Authority {
        host: String,
        port: Option<u16>,
    }

    impl Authority {
        pub fn new(host: impl Into<String>, port: Option<u16>) -> Self {
            Self {
                host: host.into(),
                port,
            }
        }

        pub fn host(&self) -> &str {
            &self.host
        }

        pub fn port(&self) -> Option<u16> {
            self.port
        }

        pub fn host_port_string(&self) -> String {
            let host_str = &self.host;
            match self.port() {
                None => host_str.to_string(),
                // Add [] for IPv6 addresses.
                Some(port) if host_str.contains(':') => {
                    format!("[{}]:{}", host_str, port)
                }
                Some(port) => format!("{}:{}", host_str, port),
            }
        }
    }
}

pub struct ProtocolInfo {
    security_protocol: &'static str,
}

impl ProtocolInfo {
    pub(crate) const fn new(security_protocol: &'static str) -> Self {
        Self { security_protocol }
    }

    pub fn security_protocol(&self) -> &'static str {
        self.security_protocol
    }
}
