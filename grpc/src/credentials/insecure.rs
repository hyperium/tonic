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
use crate::credentials::client::{
    self, ClientConnectionSecurityContext, ClientConnectionSecurityInfo, ClientHandshakeInfo,
};
use crate::credentials::common::{Authority, SecurityLevel};
use crate::credentials::server::{self, ServerConnectionSecurityInfo};
use crate::credentials::{ClientChannelCredential, ProtocolInfo, ServerChannelCredentials};
use crate::rt::{GrpcEndpoint, Runtime};

/// An implementation of [`ClientChannelCredential`] for insecure connections.
///
/// This credential type does not perform any encryption or authentication. It
/// simply passes the raw underlying transport as the output.
#[derive(Debug, Clone, Default)]
pub struct InsecureClientChannelCredentials;

impl InsecureClientChannelCredentials {
    /// Creates a new instance of `InsecureChannelCredentials`.
    pub fn new() -> Self {
        Self
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

impl client::Sealed for InsecureClientChannelCredentials {
    type ContextType = InsecureConnectionSecurityContext;
    type Output<I> = I;

    async fn connect<Input: GrpcEndpoint + 'static>(
        &self,
        _authority: &Authority<'_>,
        source: Input,
        _info: ClientHandshakeInfo,
        _runtime: Arc<dyn Runtime>,
    ) -> Result<
        (
            Self::Output<Input>,
            ClientConnectionSecurityInfo<Self::ContextType>,
        ),
        String,
    > {
        Ok((
            source,
            ClientConnectionSecurityInfo {
                security_protocol: "insecure",
                security_level: SecurityLevel::NoSecurity,
                security_context: InsecureConnectionSecurityContext,
                attributes: Attributes,
            },
        ))
    }
}

impl ClientChannelCredential for InsecureClientChannelCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo {
            security_protocol: "insecure",
        };
        &INFO
    }
}

/// An implementation of [`ServerChannelCredentials`] for insecure connections.
#[derive(Debug, Clone, Default)]
pub struct InsecureServerChannelCredentials;

impl InsecureServerChannelCredentials {
    pub fn new() -> Self {
        Self
    }
}

impl server::Sealed for InsecureServerChannelCredentials {
    type Output<I> = I;

    async fn accept<Input: GrpcEndpoint + 'static>(
        &self,
        source: Input,
        _runtime: Arc<dyn Runtime>,
    ) -> Result<(Self::Output<Input>, ServerConnectionSecurityInfo), String> {
        Ok((
            source,
            ServerConnectionSecurityInfo {
                security_protocol: "insecure",
                security_level: SecurityLevel::NoSecurity,
                attributes: Attributes,
            },
        ))
    }
}

impl ServerChannelCredentials for InsecureServerChannelCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo {
            security_protocol: "insecure",
        };
        &INFO
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::credentials::client::{
        ClientConnectionSecurityContext, ClientHandshakeInfo, Sealed as ClientSealed,
    };
    use crate::credentials::common::{Authority, SecurityLevel};
    use crate::credentials::{ClientChannelCredential, InsecureClientChannelCredentials};
    use crate::rt::GrpcEndpoint;
    use crate::rt::{self, TcpOptions};

    #[tokio::test]
    async fn test_insecure_client_credentials() {
        let creds = InsecureClientChannelCredentials::new();

        let info = creds.info();
        assert_eq!(info.security_protocol, "insecure");

        let addr = "127.0.0.1:0";
        let listener = TcpListener::bind(addr).await.unwrap();
        let server_addr = listener.local_addr().unwrap();
        let authority = Authority {
            host: "localhost",
            port: Some(server_addr.port()),
        };

        let runtime: Arc<dyn rt::Runtime> = Arc::new(rt::tokio::TokioRuntime {});
        let endpoint = runtime
            .tcp_stream(server_addr, TcpOptions::default())
            .await
            .unwrap();
        let handshake_info = ClientHandshakeInfo::default();

        let (mut endpoint, security_info) = creds
            .connect(&authority, endpoint, handshake_info, runtime)
            .await
            .unwrap();

        // Verify security info.
        assert_eq!(security_info.security_protocol, "insecure");
        assert_eq!(security_info.security_level, SecurityLevel::NoSecurity);

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
            .security_context
            .validate_authority(&authority));
    }
}
