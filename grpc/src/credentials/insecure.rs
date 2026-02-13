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
use crate::credentials::client::{
    self, ClientConnectionSecurityContext, ClientConnectionSecurityInfo, ClientHandshakeInfo,
    HandshakeOutput,
};
use crate::credentials::common::{Authority, SecurityLevel};
use crate::credentials::server::{self, ServerConnectionSecurityInfo};
use crate::credentials::{ChannelCredentials, ProtocolInfo, ServerCredentials};
use crate::rt::{GrpcEndpoint, GrpcRuntime};

/// An implementation of [`ClientChannelCredential`] for insecure connections.
///
/// This credential type does not perform any encryption or authentication. It
/// simply passes the raw underlying transport as the output.
#[derive(Debug, Clone, Default)]
pub struct InsecureChannelCredentials {
    _private: (),
}

impl InsecureChannelCredentials {
    /// Creates a new instance of `InsecureChannelCredentials`.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// An implementation of [`ClientConnectionSecurityContext`] for insecure connections.
#[derive(Debug, Clone)]
pub struct InsecureConnectionSecurityContext;

impl ClientConnectionSecurityContext for InsecureConnectionSecurityContext {
    fn validate_authority(&self, _authority: &Authority) -> bool {
        true
    }
}

impl client::ChannelCredsInternal for InsecureChannelCredentials {
    type ContextType = InsecureConnectionSecurityContext;
    type Output<I> = I;

    async fn connect<Input: GrpcEndpoint + 'static>(
        &self,
        _authority: &Authority<'_>,
        source: Input,
        _info: ClientHandshakeInfo,
        _runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<Self::Output<Input>, Self::ContextType>, String> {
        Ok(HandshakeOutput {
            endpoint: source,
            security: ClientConnectionSecurityInfo::new(
                "insecure",
                SecurityLevel::NoSecurity,
                InsecureConnectionSecurityContext,
                Attributes,
            ),
        })
    }
}

impl ChannelCredentials for InsecureChannelCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new("insecure");
        &INFO
    }
}

/// An implementation of [`ServerChannelCredentials`] for insecure connections.
#[derive(Debug, Clone, Default)]
pub struct InsecureServerCredentials {
    _private: (),
}

impl InsecureServerCredentials {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl server::ServerCredsInternal for InsecureServerCredentials {
    type Output<I> = I;

    async fn accept<Input: GrpcEndpoint + 'static>(
        &self,
        source: Input,
        _runtime: GrpcRuntime,
    ) -> Result<server::HandshakeOutput<Self::Output<Input>>, String> {
        Ok(server::HandshakeOutput {
            endpoint: source,
            security: ServerConnectionSecurityInfo::new(
                "insecure",
                SecurityLevel::NoSecurity,
                Attributes,
            ),
        })
    }
}

impl ServerCredentials for InsecureServerCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new("insecure");
        &INFO
    }
}

#[cfg(test)]
mod test {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::credentials::client::{
        ChannelCredsInternal as ClientSealed, ClientConnectionSecurityContext, ClientHandshakeInfo,
    };
    use crate::credentials::common::{Authority, SecurityLevel};
    use crate::credentials::{ChannelCredentials, InsecureChannelCredentials};
    use crate::rt::GrpcEndpoint;
    use crate::rt::{self, TcpOptions};

    #[tokio::test]
    async fn test_insecure_client_credentials() {
        let creds = InsecureChannelCredentials::new();

        let info = creds.info();
        assert_eq!(info.security_protocol(), "insecure");

        let addr = "127.0.0.1:0";
        let listener = TcpListener::bind(addr).await.unwrap();
        let server_addr = listener.local_addr().unwrap();
        let authority = Authority::new("localhost", Some(server_addr.port()));

        let runtime = rt::default_runtime();
        let endpoint = runtime
            .tcp_stream(server_addr, TcpOptions::default())
            .await
            .unwrap();
        let handshake_info = ClientHandshakeInfo::default();

        let output = creds
            .connect(&authority, endpoint, handshake_info, runtime)
            .await
            .unwrap();

        let mut endpoint = output.endpoint;
        let security_info = output.security;

        // Verify security info.
        assert_eq!(security_info.security_protocol(), "insecure");
        assert_eq!(security_info.security_level(), SecurityLevel::NoSecurity);

        // Verify data transfer.
        let (mut server_stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            endpoint.get_local_address(),
            &server_stream.peer_addr().unwrap().to_string()
        );
        let test_data = b"hello grpc";
        server_stream.write_all(test_data).await.unwrap();

        let mut buf = vec![0u8; test_data.len()];
        endpoint.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, test_data);

        // Validate arbitrary authority.
        assert!(security_info
            .security_context()
            .validate_authority(&authority));
    }
}
