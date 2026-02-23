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

pub use insecure::InsecureChannelCredentials;
pub use insecure::InsecureServerCredentials;

/// Defines the common interface for all live gRPC wire protocols and supported
/// transport security protocols (e.g., TLS, ALTS).
pub trait ChannelCredentials: client::ChannelCredsInternal + Sync + 'static {
    //// Provides the ProtocolInfo of these credentials.
    fn info(&self) -> &ProtocolInfo;
}

pub trait ServerCredentials: server::ServerCredsInternal + Sync + 'static {
    //// Provides the ProtocolInfo of this credentials.
    fn info(&self) -> &ProtocolInfo;
}

pub(crate) mod common {
    /// Defines the level of protection provided by an established connection.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// Represents the value passed as the `:authority` pseudo-header, typically
    /// in the form `host:port`.
    pub struct Authority {
        host: String,
        port: Option<u16>,
    }

    impl Authority {
        pub fn new(host: String, port: Option<u16>) -> Self {
            Self { host, port }
        }

        pub fn host(&self) -> &str {
            &self.host
        }

        pub fn port(&self) -> Option<u16> {
            self.port
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
