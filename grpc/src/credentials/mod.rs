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

pub(crate) mod client;
pub(crate) mod dyn_wrapper;
mod insecure;
pub(crate) mod server;
use tonic::async_trait;

pub use insecure::{InsecureClientChannelCredentials, InsecureServerChannelCredentials};

/// Defines the common interface for all live gRPC wire protocols and supported
/// transport security protocols (e.g., TLS, ALTS).
pub trait ClientChannelCredential: client::Sealed + Send + Sync {
    //// Provides the ProtocolInfo of this ClientChannelCredential.
    fn info(&self) -> &ProtocolInfo;
}

#[async_trait]
pub trait ServerChannelCredentials: server::Sealed + Send + Sync {
    //// Provides the ProtocolInfo of this ServerChannelCredentials.
    fn info(&self) -> &ProtocolInfo;
}

pub(crate) mod common {
    /// Defines the level of protection provided by an established connection.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum SecurityLevel {
        /// The connection is insecure; no protection is applied.
        NoSecurity,
        /// The connection guarantees both privacy (confidentiality) and data integrity.
        ///
        /// This is the standard level for secure transports like TLS.
        PrivacyAndIntegrity,
    }
    /// Represents the value passed as the `:authority` pseudo-header, typically in
    /// the form `host:port`.
    pub struct Authority<'a> {
        pub(crate) host: &'a str,
        pub(crate) port: Option<u16>,
    }
}

#[non_exhaustive]
pub struct ProtocolInfo {
    pub(crate) security_protocol: &'static str,
}
