use std::sync::Arc;
use std::sync::Mutex;

use bytes::Buf;
use bytes::Bytes;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::client::CallOptions;
use crate::client::Invoke;
use crate::client::InvokeOnce;
use crate::client::RecvStream;
use crate::client::SendOptions;
use crate::client::SendStream;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::SendMessage;

/// Implements a stream that sinks writes and only returns StreamClosed.
pub(crate) struct NopStream;
impl SendStream for NopStream {
    async fn send(&mut self, _item: &dyn SendMessage, _options: SendOptions) -> Result<(), ()> {
        Ok(())
    }
}
impl RecvStream for NopStream {
    async fn next(&mut self, _msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
        ClientResponseStreamItem::StreamClosed
    }
}

/// Implements an Invoke which only returns NopStreams.
#[derive(Clone)]
pub(crate) struct NopInvoker;
impl Invoke for NopInvoker {
    type SendStream = NopStream;
    type RecvStream = NopStream;
    async fn invoke(
        &self,
        _headers: RequestHeaders,
        _options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        (NopStream, NopStream)
    }
}

/// Implements an InvokeOnce which only returns NopStreams.
pub(crate) struct NopOnceInvoker;
impl InvokeOnce for NopOnceInvoker {
    type SendStream = NopStream;
    type RecvStream = NopStream;
    async fn invoke_once(
        self,
        _headers: RequestHeaders,
        _options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        (NopStream, NopStream)
    }
}

/// Implements a RecvMessage that does not decode.
pub(crate) struct NopRecvMessage;
impl RecvMessage for NopRecvMessage {
    fn decode(&mut self, _data: &mut dyn Buf) -> Result<(), String> {
        Ok(())
    }
}

/// Implements a RecvMessage that simply copies the data received into `data`.
pub(crate) struct ByteRecvMsg {
    pub data: Option<Bytes>,
}
impl ByteRecvMsg {
    pub fn new() -> Self {
        Self { data: None }
    }
}
impl RecvMessage for ByteRecvMsg {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String> {
        self.data = Some(data.copy_to_bytes(data.remaining()));
        Ok(())
    }
}

/// Implements a SendMessage that simply copies `data` as its output.
pub(crate) struct ByteSendMsg<'a> {
    pub data: &'a Bytes,
}
impl<'a> ByteSendMsg<'a> {
    pub fn new(data: &'a Bytes) -> Self {
        Self { data }
    }
}
impl<'a> SendMessage for ByteSendMsg<'a> {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(self.data.clone()))
    }
}

/// Implemnts an Invoker that can be controlled using the returned Controller.
#[derive(Clone)]
pub(crate) struct MockInvoker {
    pub req_headers: Arc<Mutex<Option<RequestHeaders>>>,
    pub resp_tx: broadcast::Sender<ClientResponseStreamItem>,
    pub req_tx: mpsc::Sender<(Bytes, SendOptions)>,
}
pub(crate) struct MockInvokerController {
    pub resp_tx: broadcast::Sender<ClientResponseStreamItem>,
    pub req_rx: mpsc::Receiver<(Bytes, SendOptions)>,
}
impl MockInvoker {
    pub fn new() -> (Self, MockInvokerController) {
        let (resp_tx, _) = broadcast::channel(100);
        let (req_tx, req_rx) = mpsc::channel(100);

        (
            MockInvoker {
                req_headers: Arc::new(Mutex::new(None)),
                resp_tx: resp_tx.clone(),
                req_tx,
            },
            MockInvokerController { req_rx, resp_tx },
        )
    }
}

impl MockInvokerController {
    /// Returns the next request received by the associated `MockInvoker`.
    pub async fn recv_req(&mut self) -> (Bytes, SendOptions) {
        self.req_rx.recv().await.unwrap()
    }
    /// Causes the next `RecvStream::next` call to return `item`.
    pub async fn send_resp(&mut self, item: ClientResponseStreamItem) {
        self.resp_tx.send(item).unwrap();
    }
}

impl Invoke for MockInvoker {
    type SendStream = MockSendStream;
    type RecvStream = MockRecvStream;

    async fn invoke(
        &self,
        headers: RequestHeaders,
        _options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        *self.req_headers.lock().unwrap() = Some(headers);
        (
            MockSendStream(self.req_tx.clone()),
            MockRecvStream(self.resp_tx.subscribe()),
        )
    }
}

/// Implements the SendStream for MockInvoker.
pub(crate) struct MockSendStream(pub mpsc::Sender<(Bytes, SendOptions)>);
impl SendStream for MockSendStream {
    async fn send(&mut self, item: &dyn SendMessage, options: SendOptions) -> Result<(), ()> {
        let mut data = item.encode().unwrap();
        self.0
            .send((data.copy_to_bytes(data.remaining()), options))
            .await
            .map_err(|_| ())
    }
}

/// Implements the RecvStream for MockInvoker.
pub(crate) struct MockRecvStream(pub broadcast::Receiver<ClientResponseStreamItem>);
impl RecvStream for MockRecvStream {
    async fn next(&mut self, _msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
        match self.0.recv().await {
            Ok(item) => item,
            Err(_) => ClientResponseStreamItem::StreamClosed,
        }
    }
}
