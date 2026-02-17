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

use tonic::async_trait;

use crate::credentials::client::{
    ClientConnectionSecurityContext, ClientHandshakeInfo, HandshakeOutput,
};
use crate::credentials::common::Authority;
use crate::credentials::server::HandshakeOutput as ServerHandshakeOutput;
use crate::credentials::{ChannelCredentials, ProtocolInfo, ServerCredentials};
use crate::rt::{GrpcEndpoint, GrpcRuntime};
use crate::send_future::SendFuture;

type BoxEndpoint = Box<dyn GrpcEndpoint>;

// Bridge trait for type erasure.
#[async_trait]
pub(crate) trait DynChannelCredentials: Send + Sync {
    async fn connect(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
        info: ClientHandshakeInfo,
        runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<BoxEndpoint, Box<dyn ClientConnectionSecurityContext>>, String>;

    fn info(&self) -> &ProtocolInfo;
}

#[async_trait]
impl<T> DynChannelCredentials for T
where
    T: ChannelCredentials,
    T::Output<BoxEndpoint>: GrpcEndpoint,
{
    async fn connect(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
        info: ClientHandshakeInfo,
        runtime: GrpcRuntime,
    ) -> Result<HandshakeOutput<BoxEndpoint, Box<dyn ClientConnectionSecurityContext>>, String>
    {
        let output = self
            .connect(authority, source, info, runtime)
            .make_send()
            .await?;

        let stream = output.endpoint;
        let sec_info = output.security;

        Ok(HandshakeOutput {
            endpoint: Box::new(stream),
            security: sec_info.into_boxed(),
        })
    }

    fn info(&self) -> &ProtocolInfo {
        self.info()
    }
}

// Bridge trait for type erasure.
#[async_trait]
pub(crate) trait DynServerCredentials: Send + Sync {
    async fn accept(
        &self,
        source: BoxEndpoint,
        runtime: GrpcRuntime,
    ) -> Result<ServerHandshakeOutput<BoxEndpoint>, String>;

    fn info(&self) -> &ProtocolInfo;
}

#[async_trait]
impl<T> DynServerCredentials for T
where
    T: ServerCredentials,
    T::Output<BoxEndpoint>: GrpcEndpoint,
{
    async fn accept(
        &self,
        source: BoxEndpoint,
        runtime: GrpcRuntime,
    ) -> Result<ServerHandshakeOutput<BoxEndpoint>, String> {
        let output = SendFuture::make_send(self.accept(source, runtime)).await?;
        Ok(ServerHandshakeOutput {
            endpoint: Box::new(output.endpoint),
            security: output.security,
        })
    }

    fn info(&self) -> &ProtocolInfo {
        self.info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::client::ClientHandshakeInfo;
    use crate::credentials::common::{Authority, SecurityLevel};
    use crate::credentials::insecure::InsecureChannelCredentials;
    use crate::rt::TcpOptions;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_dyn_client_credential_dispatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let creds = InsecureChannelCredentials::new();
        let dyn_creds: Box<dyn DynChannelCredentials> = Box::new(creds);

        let authority = Authority::new("localhost".to_string(), Some(addr.port()));

        let runtime = crate::rt::default_runtime();
        let source = runtime
            .tcp_stream(addr, TcpOptions::default())
            .await
            .unwrap();
        let info = ClientHandshakeInfo::default();

        let result = dyn_creds.connect(&authority, source, info, runtime).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        let mut endpoint = output.endpoint;
        let security_info = output.security;

        assert!(!endpoint.get_local_address().is_empty());
        assert_eq!(security_info.security_protocol(), "insecure");
        assert_eq!(security_info.security_level(), SecurityLevel::NoSecurity);

        // Verify data transfer.
        let (mut server_stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            endpoint.get_local_address(),
            &server_stream.peer_addr().unwrap().to_string()
        );
        let test_data = b"hello dynamic grpc";
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
