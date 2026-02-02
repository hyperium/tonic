use std::sync::Arc;

use tokio::sync::oneshot;
use tonic::async_trait;

use crate::{
    core::{RecvMessage, RequestHeaders, ServerResponseStreamItem},
    service::{Request, Response, Service},
};

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
        _method: String,
        _headers: RequestHeaders,
        tx: &impl SendStream,
        rx: impl RecvStream + 'static,
    );
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
    async fn send(
        &mut self,
        item: ServerResponseStreamItem,
        options: SendOptions,
    ) -> Result<(), ()>;
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
