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

use std::net::SocketAddr;
use std::str::FromStr;

use crate::attributes::Attributes;
use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
use crate::credentials::ChannelCredentials;
use crate::credentials::ProtocolInfo;
use crate::credentials::ServerCredentials;
use crate::credentials::client;
use crate::credentials::client::ClientConnectionSecurityContext;
use crate::credentials::client::ClientConnectionSecurityInfo;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::client::HandshakeOutput;
use crate::credentials::common::Authority;
use crate::credentials::common::SecurityLevel;
use crate::credentials::server;
use crate::credentials::server::ServerConnectionSecurityInfo;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;

pub const PROTOCOL_NAME: &str = "local";

/// An implementation of [`ChannelCredentials`] for connections on the same
/// machine.
///
/// This credential type does not perform any encryption or authentication. It
/// simply passes the raw underlying transport as the output.
#[derive(Debug, Clone, Default)]
pub struct LocalChannelCredentials {
    _private: (),
}

impl LocalChannelCredentials {
    /// Creates a new instance of `InsecureChannelCredentials`.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// An implementation of [`ClientConnectionSecurityContext`] for local
/// connections.
#[derive(Debug, Clone)]
pub struct LocalConnectionSecurityContext;

impl ClientConnectionSecurityContext for LocalConnectionSecurityContext {
    fn validate_authority(&self, _authority: &Authority) -> bool {
        true
    }
}

/// Returns the security level for a local connection.
/// It returns an error if a connection is not local.
/// Refer to L62: https://github.com/grpc/proposal/blob/master/L62-core-call-credential-security-level.md
fn security_level_for_endpoint(
    peer_addr: &str,
    network_type: &str,
) -> Result<SecurityLevel, String> {
    if network_type == TCP_IP_NETWORK_TYPE
        && SocketAddr::from_str(peer_addr)
            .map_err(|e| e.to_string())?
            .ip()
            .is_loopback()
    {
        return Ok(SecurityLevel::NoSecurity);
    }
    // TODO: Add support for unix sockets.
    Err(format!(
        "local credentials rejected connection to non-local address {}",
        peer_addr
    ))
}

impl client::ChannelCredsInternal for LocalChannelCredentials {
    type ContextType = LocalConnectionSecurityContext;
    type Output<I> = I;

    async fn connect<Input: GrpcEndpoint>(
        &self,
        _authority: &Authority,
        source: Input,
        _info: ClientHandshakeInfo,
        _runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<Self::Output<Input>, Self::ContextType>, String> {
        let security_level =
            security_level_for_endpoint(source.get_peer_address(), source.get_network_type())?;
        Ok(HandshakeOutput {
            endpoint: source,
            security: ClientConnectionSecurityInfo::new(
                PROTOCOL_NAME,
                security_level,
                LocalConnectionSecurityContext,
                Attributes,
            ),
        })
    }
}

impl ChannelCredentials for LocalChannelCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new(PROTOCOL_NAME);
        &INFO
    }
}

/// An implementation of [`ServerCredentials`] for local connections.
#[derive(Debug, Clone, Default)]
pub struct LocalServerCredentials {
    _private: (),
}

impl LocalServerCredentials {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl server::ServerCredsInternal for LocalServerCredentials {
    type Output<I> = I;

    async fn accept<Input: GrpcEndpoint>(
        &self,
        source: Input,
        _runtime: GrpcRuntime,
    ) -> Result<server::HandshakeOutput<Self::Output<Input>>, String> {
        let security_level =
            security_level_for_endpoint(source.get_peer_address(), source.get_network_type())?;
        Ok(server::HandshakeOutput {
            endpoint: source,
            security: ServerConnectionSecurityInfo::new(PROTOCOL_NAME, security_level, Attributes),
        })
    }
}

impl ServerCredentials for LocalServerCredentials {
    fn info(&self) -> &ProtocolInfo {
        static INFO: ProtocolInfo = ProtocolInfo::new(PROTOCOL_NAME);
        &INFO
    }
}

#[cfg(test)]
mod test {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::net::TcpStream;

    use super::*;
    use crate::credentials::ChannelCredentials;
    use crate::credentials::ServerCredentials;
    use crate::credentials::client::ChannelCredsInternal as ClientSealed;
    use crate::credentials::client::ClientConnectionSecurityContext;
    use crate::credentials::client::ClientHandshakeInfo;
    use crate::credentials::common::Authority;
    use crate::credentials::common::SecurityLevel;
    use crate::credentials::server::ServerCredsInternal;
    use crate::rt;
    use crate::rt::GrpcEndpoint;
    use crate::rt::TcpOptions;

    #[test]
    fn test_security_level_for_endpoint_success() {
        assert_eq!(
            security_level_for_endpoint("127.0.0.1:8080", TCP_IP_NETWORK_TYPE),
            Ok(SecurityLevel::NoSecurity)
        );
        assert_eq!(
            security_level_for_endpoint("[::1]:8080", TCP_IP_NETWORK_TYPE),
            Ok(SecurityLevel::NoSecurity)
        );
    }

    #[test]
    fn test_security_level_for_endpoint_failure() {
        assert!(security_level_for_endpoint("192.168.1.1:8080", TCP_IP_NETWORK_TYPE).is_err());
        assert!(security_level_for_endpoint("127.0.0.1:8080", "unix").is_err());
        assert!(security_level_for_endpoint("invalid", TCP_IP_NETWORK_TYPE).is_err());
    }

    #[tokio::test]
    async fn test_local_client_credentials() {
        let creds = LocalChannelCredentials::new();

        let info = creds.info();
        assert_eq!(info.security_protocol(), "local");

        let addr = "127.0.0.1:0";
        let listener = TcpListener::bind(addr).await.unwrap();
        let server_addr = listener.local_addr().unwrap();
        let authority = Authority::new("localhost".to_string(), Some(server_addr.port()));

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
        assert_eq!(security_info.security_protocol(), "local");
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
        assert!(
            security_info
                .security_context()
                .validate_authority(&authority)
        );
    }

    #[tokio::test]
    async fn test_local_server_credentials() {
        let creds = LocalServerCredentials::new();

        let info = creds.info();
        assert_eq!(info.security_protocol, "local");

        let addr = "127.0.0.1:0";
        let runtime = rt::default_runtime();
        let mut listener = runtime
            .listen_tcp(addr.parse().unwrap(), TcpOptions::default())
            .await
            .unwrap();
        let server_addr = *listener.local_addr();

        let client_handle = tokio::spawn(async move {
            let mut stream = TcpStream::connect(server_addr).await.unwrap();
            let data = b"hello grpc";
            stream.write_all(data).await.unwrap();

            // Keep the connection alive for a bit so server can read.
            let mut buf = vec![0u8; 1];
            let _ = stream.read(&mut buf).await;
        });

        let (server_stream, _) = listener.accept().await.unwrap();

        let output = creds.accept(server_stream, runtime).await.unwrap();
        let mut endpoint = output.endpoint;
        let security_info = output.security;

        assert_eq!(security_info.security_protocol(), "local");
        assert_eq!(security_info.security_level(), SecurityLevel::NoSecurity);

        let mut buf = vec![0u8; 10];
        endpoint.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf[..], b"hello grpc");

        client_handle.abort();
    }
}
