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
use crate::credentials::common::SecurityLevel;
use crate::rt::{GrpcEndpoint, GrpcRuntime};

#[trait_variant::make(Send)]
pub trait ServerCredsInternal {
    type Output<I>;
    /// Performs the server-side authentication handshake.
    ///
    /// This method wraps the incoming raw `source` connection with the configured
    /// security protocol (e.g., TLS).
    ///
    /// # Returns
    ///
    /// A tuple containing:
    /// 1. The authenticated endpoint (ready for reading/writing frames).
    async fn accept<Input: GrpcEndpoint>(
        &self,
        source: Input,
        runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<Self::Output<Input>>, String>;
}

pub struct HandshakeOutput<T> {
    pub endpoint: T,
    pub security: ServerConnectionSecurityInfo,
}

/// Represents the security state of an established server-side connection.
pub struct ServerConnectionSecurityInfo {
    security_protocol: &'static str,
    security_level: SecurityLevel,
    /// Stores extra data derived from the underlying protocol.
    attributes: Attributes,
}

impl ServerConnectionSecurityInfo {
    /// Creates a new instance of `ServerConnectionSecurityInfo`.
    pub fn new(
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

    /// Returns the security protocol used.
    pub fn security_protocol(&self) -> &'static str {
        self.security_protocol
    }

    /// Returns the security level of the connection.
    pub fn security_level(&self) -> SecurityLevel {
        self.security_level
    }

    /// Returns the attributes associated with the connection.
    pub fn attributes(&self) -> &Attributes {
        &self.attributes
    }
}
