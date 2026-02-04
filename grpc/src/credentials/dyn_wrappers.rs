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

use crate::{
    credentials::{
        client::{
            ClientConnectionSecurityContext, ClientConnectionSecurityInfo, ClientHandshakeInfo,
        },
        server::ServerConnectionSecurityInfo,
        Authority, ClientChannelCredential, ProtocolInfo, ServerChannelCredentials,
    },
    rt::{GrpcEndpoint, Runtime},
};

impl ClientConnectionSecurityContext for Box<dyn ClientConnectionSecurityContext> {}
type BoxEndpoint = Box<dyn GrpcEndpoint>;

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
        let (stream, sec_info) = self.connect(authority, source, info, runtime).await?;

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
        source: Box<dyn GrpcEndpoint>,
        runtime: Arc<dyn Runtime>,
    ) -> Result<(Box<dyn GrpcEndpoint>, ServerConnectionSecurityInfo), String> {
        let (stream, sec_info) = self.accept(source, runtime).await?;
        let boxed_stream: Box<dyn GrpcEndpoint> = Box::new(stream);
        Ok((boxed_stream, sec_info))
    }

    fn info(&self) -> &ProtocolInfo {
        self.info()
    }
}
