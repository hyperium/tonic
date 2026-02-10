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

use crate::credentials::client::{
    ClientConnectionSecurityContext, ClientConnectionSecurityInfo, ClientHandshakeInfo,
};
use crate::credentials::common::Authority;
use crate::credentials::server::ServerConnectionSecurityInfo;
use crate::credentials::{ClientChannelCredential, ProtocolInfo, ServerChannelCredentials};
use crate::rt::{GrpcEndpoint, Runtime};
use crate::send_future::SendFuture;

impl ClientConnectionSecurityContext for Box<dyn ClientConnectionSecurityContext> {
    fn validate_authority(&self, authority: &Authority) -> bool {
        (**self).validate_authority(authority)
    }
}
type BoxEndpoint = Box<dyn GrpcEndpoint + 'static>;

// Bridge trait for type erasure.
#[async_trait]
pub(crate) trait DynClientChannelCredential: Send + Sync {
    async fn connect(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
        info: ClientHandshakeInfo,
        runtime: Arc<dyn Runtime>,
    ) -> Result<
        (
            Box<dyn GrpcEndpoint>,
            ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>,
        ),
        String,
    >;

    fn info(&self) -> &ProtocolInfo;
}

#[async_trait]
impl<T> DynClientChannelCredential for T
where
    T: ClientChannelCredential,
    T::Output<BoxEndpoint>: GrpcEndpoint + 'static,
{
    async fn connect(
        &self,
        authority: &Authority,
        source: BoxEndpoint,
        info: ClientHandshakeInfo,
        runtime: Arc<dyn Runtime>,
    ) -> Result<
        (
            BoxEndpoint,
            ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>,
        ),
        String,
    > {
        let (stream, sec_info) = self
            .connect(authority, source, info, runtime)
            .make_send()
            .await?;

        let boxed_stream: BoxEndpoint = Box::new(stream);

        let sec_info = ClientConnectionSecurityInfo {
            security_protocol: sec_info.security_protocol,
            security_level: sec_info.security_level,
            security_context: Box::new(sec_info.security_context)
                as Box<dyn ClientConnectionSecurityContext>,
            attributes: sec_info.attributes,
        };

        Ok((boxed_stream, sec_info))
    }

    fn info(&self) -> &ProtocolInfo {
        self.info()
    }
}

// Bridge trait for type erasure.
#[async_trait]
pub(crate) trait DynServerChannelCredentials: Send + Sync {
    async fn accept(
        &self,
        source: BoxEndpoint,
        runtime: Arc<dyn Runtime>,
    ) -> Result<(BoxEndpoint, ServerConnectionSecurityInfo), String>;

    fn info(&self) -> &ProtocolInfo;
}

#[async_trait]
impl<T> DynServerChannelCredentials for T
where
    T: ServerChannelCredentials,
    T::Output<BoxEndpoint>: GrpcEndpoint + 'static,
{
    async fn accept(
        &self,
        source: BoxEndpoint,
        runtime: Arc<dyn Runtime>,
    ) -> Result<(BoxEndpoint, ServerConnectionSecurityInfo), String> {
        let (stream, sec_info) = SendFuture::make_send(self.accept(source, runtime)).await?;
        let boxed_stream: Box<dyn GrpcEndpoint> = Box::new(stream);
        Ok((boxed_stream, sec_info))
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
    use crate::credentials::insecure::InsecureClientChannelCredentials;
    use crate::rt::tokio::TokioRuntime;
    use crate::rt::TcpOptions;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_dyn_client_credential_dispatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let creds = InsecureClientChannelCredentials::new();
        let dyn_creds: Box<dyn DynClientChannelCredential> = Box::new(creds);

        let authority = Authority {
            host: "localhost",
            port: Some(addr.port()),
        };

        let runtime = Arc::new(TokioRuntime {});
        let source = runtime
            .tcp_stream(addr, TcpOptions::default())
            .await
            .unwrap();
        let info = ClientHandshakeInfo::default();

        let result = dyn_creds.connect(&authority, source, info, runtime).await;

        assert!(result.is_ok());
        let (mut endpoint, security_info) = result.unwrap();

        assert!(!endpoint.get_local_address().is_empty());
        assert_eq!(security_info.security_protocol, "insecure");
        assert_eq!(security_info.security_level, SecurityLevel::NoSecurity);

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
            .security_context
            .validate_authority(&authority));
    }
}
