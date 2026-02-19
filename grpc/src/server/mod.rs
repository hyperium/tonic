/*
 *
 * Copyright 2025 gRPC authors.
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

use tokio::sync::oneshot;
use tonic::async_trait;

use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::ServerResponseStreamItem;
use crate::service::Request;
use crate::service::Response;
use crate::service::Service;

pub struct Server {
    handler: Option<Arc<dyn Service>>,
}

pub type Call = (String, Request, oneshot::Sender<Response>);

#[async_trait]
pub trait Listener {
    async fn accept(&self) -> Option<Call>;
}

impl Server {
    pub fn new() -> Self {
        Self { handler: None }
    }

    pub fn set_handler(&mut self, f: impl Service + 'static) {
        self.handler = Some(Arc::new(f))
    }

    pub async fn serve(&self, l: &impl Listener) {
        while let Some((method, req, reply_on)) = l.accept().await {
            reply_on
                .send(self.handler.as_ref().unwrap().call(method, req).await)
                .ok(); // TODO: log error
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

/// A trait which may be implemented by types to handle server-side logic of
/// RPCs (Remote Procedure Calls, often shortened to "call").
#[trait_variant::make(Send)]
pub trait Handle: Send + Sync {
    /// Handles an RPC, accepting the send and receive streams that are used to
    /// interact with the call.  Note that `tx` is not static, so it cannot be
    /// sent to another task, meaning the RPC must end before handle returns.
    async fn handle(
        &self,
        headers: RequestHeaders,
        tx: &impl SendStream,
        rx: impl RecvStream + 'static,
    );
}

#[async_trait]
pub trait DynHandle: Send + Sync {
    async fn dyn_handle(
        &self,
        headers: RequestHeaders,
        tx: &mut dyn DynSendStream,
        rx: Box<dyn DynRecvStream>,
    );
}

#[async_trait]
impl<T: Handle> DynHandle for T {
    async fn dyn_handle(
        &self,
        headers: RequestHeaders,
        mut tx: &mut dyn DynSendStream,
        rx: Box<dyn DynRecvStream>,
    ) {
        self.handle(headers, &mut tx, rx).await
    }
}

/// Represents the sending side of a server stream.  See `ResponseStream`
/// documentation for information about the different types of items and the
/// order in which they must be sent.
#[trait_variant::make(Send)]
pub trait SendStream {
    /// Sends the next item on the stream.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the SendStream are undefined and data may be lost.
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()>;
}

#[async_trait]
pub trait DynSendStream: Send {
    async fn dyn_send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()>;
}

#[async_trait]
impl<T: SendStream> DynSendStream for T {
    async fn dyn_send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        self.send(item, options).await
    }
}

impl SendStream for &mut dyn DynSendStream {
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        (**self).dyn_send(item, options).await
    }
}

impl SendStream for Box<dyn DynSendStream> {
    async fn send<'a>(
        &mut self,
        item: ServerResponseStreamItem<'a>,
        options: SendOptions,
    ) -> Result<(), ()> {
        (**self).dyn_send(item, options).await
    }
}

/// Contains settings to configure a send operation on a SendStream.
#[derive(Default)]
#[non_exhaustive]
pub struct SendOptions {
    /// Delays sending the message until the trailers are provided on the stream
    /// and batches the two items together if possible.
    pub final_msg: bool,
    /// If set, compression will be disabled for this message.
    pub disable_compression: bool,
}

/// Represents the receiving side of a server stream.
#[trait_variant::make(Send)]
pub trait RecvStream {
    /// Returns the next message on the stream.  If an error is returned, the
    /// stream ended or the client closed the send side of the request stream.
    ///
    /// # Cancel safety
    ///
    /// This method is not intended to be cancellation safe.  If the returned
    /// future is not polled to completion, the behavior of any subsequent calls
    /// to the RecvStream are undefined and data may be lost.
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Result<(), ()>;
}

#[async_trait]
pub trait DynRecvStream: Send {
    async fn dyn_next(&mut self, msg: &mut dyn RecvMessage) -> Result<(), ()>;
}

#[async_trait]
impl<T: RecvStream> DynRecvStream for T {
    async fn dyn_next(&mut self, msg: &mut dyn RecvMessage) -> Result<(), ()> {
        self.next(msg).await
    }
}

impl RecvStream for Box<dyn DynRecvStream> {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Result<(), ()> {
        (**self).dyn_next(msg).await
    }
}
