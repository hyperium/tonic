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

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use bytes::Buf;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::StatusCodeError;
use crate::StatusError;
use crate::attributes::Attributes;
use crate::byte_str::ByteStr;
use crate::client::CallOptions;
use crate::client::DynRecvStream as ClientDynRecvStream;
use crate::client::DynSendStream as ClientDynSendStream;
use crate::client::Invoke;
use crate::client::RecvStream as ClientRecvStream;
use crate::client::RequestHeaders as ClientRequestHeaders;
use crate::client::ResponseHeaders as ClientResponseHeaders;
use crate::client::ResponseStreamItem;
use crate::client::SendOptions as ClientSendOptions;
use crate::client::SendStream as ClientSendStream;
use crate::client::Trailers as ClientTrailers;
use crate::client::name_resolution::ChannelController as ResolverChannelController;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::Resolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverOptions;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::global_registry as global_resolver_registry;
use crate::client::transport::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::SecurityOpts;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::core::Address;
use crate::core::ConnectionInfo;
use crate::core::RecvMessage;
use crate::core::SendMessage;
use crate::credentials::SecurityInfo;
use crate::credentials::SecurityLevel;
use crate::rt::GrpcRuntime;
use crate::server::DynHandle;
use crate::server::GracefulConnection;
use crate::server::Listener;
use crate::server::RecvStream as ServerRecvStream;
use crate::server::RequestHeaders as ServerRequestHeaders;
use crate::server::ResponseHeaders as ServerResponseHeaders;
use crate::server::ResponseStreamItem as ServerResponseStreamItem;
use crate::server::SendOptions as ServerSendOptions;
use crate::server::SendStream as ServerSendStream;
use crate::server::Trailers as ServerTrailers;
use crate::server::Transport as ServerTransport;

static LISTENERS: LazyLock<Mutex<HashMap<String, mpsc::Sender<InMemoryServerCall>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct InMemoryServerCall {
    headers: ServerRequestHeaders,
    req_rx: mpsc::UnboundedReceiver<InMemoryRequestStreamItem>,
    resp_tx: mpsc::UnboundedSender<InMemoryResponseStreamItem>,
    trailer_tx: oneshot::Sender<ServerTrailers>,
}

enum InMemoryRequestStreamItem {
    Message(Box<dyn Buf + Send + Sync>),
    StreamClosed,
}

enum InMemoryResponseStreamItem {
    Headers(ServerResponseHeaders),
    Message(Box<dyn Buf + Send + Sync>),
}
#[derive(Debug)]
struct InMemoryListenerAddress {
    id: String,
}

impl crate::rt::address::ListenerAddress for InMemoryListenerAddress {
    fn network(&self) -> &str {
        "inmemory"
    }
}

#[derive(Clone)]
pub struct InMemoryListener {
    inner: Arc<InMemoryListenerInner>,
}

struct InMemoryListenerInner {
    id: String,
    r: TokioMutex<mpsc::Receiver<InMemoryServerCall>>,
    close_notify: Arc<Notify>,
    drop_notify: Arc<Notify>,
}

impl Drop for InMemoryListenerInner {
    fn drop(&mut self) {
        LISTENERS.lock().unwrap().remove(&self.id);
        self.drop_notify.notify_waiters();
    }
}

impl Default for InMemoryListener {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryListener {
    pub fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let (s, r) = mpsc::channel(1);
        let mut listeners = LISTENERS.lock().unwrap();
        listeners.insert(id.clone(), s);
        Self {
            inner: Arc::new(InMemoryListenerInner {
                id,
                r: TokioMutex::new(r),
                close_notify: Arc::new(Notify::new()),
                drop_notify: Arc::new(Notify::new()),
            }),
        }
    }

    pub fn id(&self) -> String {
        self.inner.id.clone()
    }

    pub async fn close(self) {
        let id = self.inner.id.clone();
        let drop_notify = self.inner.drop_notify.clone();
        let weak = Arc::downgrade(&self.inner);

        LISTENERS.lock().unwrap().remove(&id);

        self.inner.close_notify.notify_waiters();

        drop(self);

        loop {
            let notified = drop_notify.notified();
            if weak.upgrade().is_none() {
                return;
            }
            notified.await;
        }
    }

    pub async fn await_connection(&self) {}
}

impl Listener for InMemoryListener {
    type Transport = InMemoryServerCall;

    async fn accept(&self, _token: crate::private::Internal) -> Option<Self::Transport> {
        let mut r = self.inner.r.lock().await;
        tokio::select! {
            call = r.recv() => {
                call
            }
            _ = self.inner.close_notify.notified() => {
                None
            }
        }
    }

    fn local_addr(&self) -> Box<dyn crate::rt::address::ListenerAddress> {
        Box::new(InMemoryListenerAddress {
            id: self.inner.id.clone(),
        })
    }
}

/// A serving in-memory connection.
pub struct InMemoryServingConnection {
    inner: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

impl std::future::Future for InMemoryServingConnection {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.inner.as_mut().poll(cx)
    }
}

impl GracefulConnection for InMemoryServingConnection {
    fn graceful_shutdown(self: Pin<&mut Self>, _token: crate::private::Internal) {
        // No-op: each in-memory "connection" is a single RPC.
    }
}

impl ServerTransport for InMemoryServerCall {
    type Connection = InMemoryServingConnection;

    fn serve(
        self,
        handler: Arc<dyn DynHandle>,
        _runtime: GrpcRuntime,
        _token: crate::private::Internal,
    ) -> InMemoryServingConnection {
        let mut send = InMemoryServerSendStream { tx: self.resp_tx };
        let recv = Box::new(InMemoryServerRecvStream { rx: self.req_rx });
        let options = crate::server::CallOptions::default();
        let trailers_tx = self.trailer_tx;

        let inner = Box::pin(async move {
            let trailers = handler
                .dyn_handle(self.headers, options, &mut send, recv)
                .await;
            let _ = trailers_tx.send(trailers);
        });
        InMemoryServingConnection { inner }
    }
}

pub struct InMemoryServerSendStream {
    tx: mpsc::UnboundedSender<InMemoryResponseStreamItem>,
}

impl ServerSendStream for InMemoryServerSendStream {
    async fn send(
        &mut self,
        item: ServerResponseStreamItem<'_>,
        _options: ServerSendOptions,
    ) -> Result<(), ()> {
        let inmemory_item = match item {
            ServerResponseStreamItem::Headers(h) => InMemoryResponseStreamItem::Headers(h),
            ServerResponseStreamItem::Message(m) => {
                let buf = m.encode().map_err(|_| ())?;
                InMemoryResponseStreamItem::Message(buf)
            }
        };

        self.tx.send(inmemory_item).map_err(|_| ())
    }
}

pub struct InMemoryServerRecvStream {
    rx: mpsc::UnboundedReceiver<InMemoryRequestStreamItem>,
}

impl ServerRecvStream for InMemoryServerRecvStream {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Option<Result<(), ()>> {
        match self.rx.recv().await {
            Some(InMemoryRequestStreamItem::Message(mut buf)) => {
                if msg.decode(&mut buf).is_err() {
                    return Some(Err(()));
                }
                Some(Ok(()))
            }
            Some(InMemoryRequestStreamItem::StreamClosed) => None,
            None => None,
        }
    }
}

pub struct InMemoryConnection {
    s: mpsc::Sender<InMemoryServerCall>,
    closed_tx: Option<oneshot::Sender<Result<(), String>>>,
    connection_info: ConnectionInfo,
}

impl Invoke for InMemoryConnection {
    type SendStream = Box<dyn ClientDynSendStream>;
    type RecvStream = Box<dyn ClientDynRecvStream>;

    async fn invoke(
        &self,
        headers: ClientRequestHeaders,
        _options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        let (req_tx, req_rx) = mpsc::unbounded_channel::<InMemoryRequestStreamItem>();
        let (resp_tx, resp_rx) = mpsc::unbounded_channel::<InMemoryResponseStreamItem>();
        let (trailer_tx, trailer_rx) = oneshot::channel();

        let (method_name, metadata) = headers.into_parts();
        let server_headers = ServerRequestHeaders::new(method_name, self.connection_info.clone())
            .with_metadata(metadata);

        let call = InMemoryServerCall {
            headers: server_headers,
            req_rx,
            resp_tx,
            trailer_tx,
        };

        let _ = self.s.send(call).await;

        (
            Box::new(InMemoryClientSendStream { tx: Some(req_tx) }),
            Box::new(InMemoryClientRecvStream {
                rx: resp_rx,
                trailer_rx: Some(trailer_rx),
                connection_info: Some(self.connection_info.clone()),
            }),
        )
    }
}
impl Drop for InMemoryConnection {
    fn drop(&mut self) {
        let _ = self.closed_tx.take().unwrap().send(Err("".into()));
    }
}

pub struct InMemoryClientSendStream {
    tx: Option<mpsc::UnboundedSender<InMemoryRequestStreamItem>>,
}

impl ClientSendStream for InMemoryClientSendStream {
    async fn send(&mut self, msg: &dyn SendMessage, _options: ClientSendOptions) -> Result<(), ()> {
        let buf = msg.encode().unwrap();

        if self
            .tx
            .as_mut()
            .unwrap()
            .send(InMemoryRequestStreamItem::Message(buf))
            .is_err()
        {
            self.tx = None;
            return Err(());
        }
        Ok(())
    }
}

impl Drop for InMemoryClientSendStream {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(InMemoryRequestStreamItem::StreamClosed);
        }
    }
}

pub struct InMemoryClientRecvStream {
    rx: mpsc::UnboundedReceiver<InMemoryResponseStreamItem>,
    trailer_rx: Option<oneshot::Receiver<ServerTrailers>>,
    connection_info: Option<ConnectionInfo>,
}

impl ClientRecvStream for InMemoryClientRecvStream {
    async fn recv(&mut self, msg: &mut dyn RecvMessage) -> ResponseStreamItem {
        match self.rx.recv().await {
            Some(InMemoryResponseStreamItem::Headers(h)) => {
                // Note: connection_info is always set when the stream is created, and
                // the server should not send headers twice, so expect here
                // should be safe.
                ResponseStreamItem::Headers(
                    ClientResponseHeaders::new(
                        self.connection_info
                            .take()
                            .expect("stream should have connection_info"),
                    )
                    .with_metadata(h.into_metadata()),
                )
            }
            Some(InMemoryResponseStreamItem::Message(mut buf)) => {
                msg.decode(&mut buf).unwrap();
                ResponseStreamItem::Message
            }
            _ => {
                if let Some(trailer_rx) = self.trailer_rx.take() {
                    match trailer_rx.await {
                        Ok(trailers) => {
                            let (status, metadata) = trailers.into_parts();
                            let client_trailers = ClientTrailers::new(status)
                                .with_metadata(metadata)
                                .with_connection_info(self.connection_info.take());
                            return ResponseStreamItem::Trailers(client_trailers);
                        }
                        Err(_) => {
                            return ResponseStreamItem::Trailers(ClientTrailers::new(Err(
                                StatusError::new(
                                    StatusCodeError::Internal,
                                    "stream ended without trailers in in-memory transport",
                                ),
                            )));
                        }
                    }
                }
                ResponseStreamItem::StreamClosed
            }
        }
    }
}

pub struct InMemoryTransport {}

impl Transport for InMemoryTransport {
    type Service = InMemoryConnection;

    async fn connect(
        &self,
        address: &Address,
        _runtime: GrpcRuntime,
        _security_opts: &SecurityOpts,
        _options: &TransportOptions,
    ) -> Result<
        (
            Self::Service,
            ConnectionInfo,
            oneshot::Receiver<Result<(), String>>,
        ),
        String,
    > {
        let target = &*address.address;
        let listeners = LISTENERS.lock().unwrap();
        let s = listeners
            .get(target)
            .ok_or_else(|| format!("no listener for target: {}", target))?
            .clone();

        let (closed_tx, closed_rx) = oneshot::channel();
        let sec_info =
            SecurityInfo::new("inmemory").with_security_level(SecurityLevel::PrivacyAndIntegrity);
        let local_address = Address {
            network_type: address.network_type,
            address: ByteStr::default(),
            attributes: Attributes::new(),
        };
        let connection_info = ConnectionInfo::new(local_address, address.clone(), sec_info);
        let conn = InMemoryConnection {
            s: s.clone(),
            closed_tx: Some(closed_tx),
            connection_info: connection_info.clone(),
        };

        Ok((conn, connection_info, closed_rx))
    }
}

pub struct InMemoryResolverBuilder {}

impl ResolverBuilder for InMemoryResolverBuilder {
    fn build(&self, target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
        let path = target.path().strip_prefix('/').unwrap_or(target.path());
        let ids: Vec<String> = path.split(',').map(|s| s.to_string()).collect();
        options.work_scheduler.schedule_work();
        Box::new(InMemoryResolver { ids })
    }

    fn scheme(&self) -> &str {
        "inmemory"
    }

    fn is_valid_uri(&self, _uri: &Target) -> bool {
        true
    }
}

struct InMemoryResolver {
    ids: Vec<String>,
}

impl Resolver for InMemoryResolver {
    fn resolve_now(&mut self) {}

    fn work(&mut self, channel_controller: &mut dyn ResolverChannelController) {
        let endpoints = self
            .ids
            .iter()
            .map(|id| Endpoint {
                addresses: vec![Address {
                    network_type: "inmemory",
                    address: crate::byte_str::ByteStr::from(id.clone()),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect();

        let service_config = channel_controller
            .parse_service_config(r#"{"loadBalancingConfig": [{"round_robin": {}}]}"#)
            .map(Some);

        let _ = channel_controller.update(ResolverUpdate {
            endpoints: Ok(endpoints),
            service_config,
            ..Default::default()
        });
    }
}

pub fn reg() {
    GLOBAL_TRANSPORT_REGISTRY.add_transport("inmemory", InMemoryTransport {});
    global_resolver_registry().add_builder(Box::new(InMemoryResolverBuilder {}));
}

#[cfg(test)]
mod tests {
    use bytes::Buf;

    use super::*;
    use crate::core::RecvMessage;
    use crate::core::test_connection_info;

    struct NopRecvMessage;
    impl RecvMessage for NopRecvMessage {
        fn decode(&mut self, _data: &mut dyn Buf) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_in_memory_recv_stream_missing_trailers() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InMemoryResponseStreamItem>();
        // Drop sender immediately so rx returns None
        drop(tx);

        let (trailer_tx, trailer_rx) = tokio::sync::oneshot::channel::<ServerTrailers>();
        // Drop trailer_tx without sending to simulate failure
        drop(trailer_tx);

        let mut stream = InMemoryClientRecvStream {
            rx,
            trailer_rx: Some(trailer_rx),
            connection_info: Some(test_connection_info()),
        };

        let mut msg = NopRecvMessage;
        let item = stream.recv(&mut msg).await;

        match item {
            ResponseStreamItem::Trailers(t) => {
                assert_eq!(
                    t.status().as_ref().unwrap_err().code(),
                    crate::StatusCodeError::Internal
                );
                assert!(
                    t.status()
                        .as_ref()
                        .unwrap_err()
                        .message()
                        .contains("stream ended without trailers")
                );
            }
            _ => panic!("expected trailers with error, got {:?}", item),
        }
    }

    #[test]
    fn in_memory_listener_local_addr() {
        use crate::server::Listener;
        let listener = InMemoryListener::new();
        let addr = listener.local_addr();
        assert_eq!(addr.network(), "inmemory");
        let debug = format!("{addr:?}");
        assert!(
            debug.contains("id:"),
            "expected debug to include id field, got: {debug}"
        );
    }

    #[test]
    fn in_memory_listener_id_is_unique() {
        let a = InMemoryListener::new();
        let b = InMemoryListener::new();
        assert_ne!(a.id(), b.id());
    }

    #[tokio::test]
    async fn inmemory_handler_cancelled_on_connection_drop() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::sync::atomic::Ordering;

        use crate::server::CallOptions;
        use crate::server::RecvStream;
        use crate::server::RequestHeaders;
        use crate::server::SendStream;
        use crate::server::Trailers;

        let handler_started = Arc::new(AtomicBool::new(false));
        let handler_finished = Arc::new(AtomicBool::new(false));
        let hs = handler_started.clone();
        let hf = handler_finished.clone();

        struct HangingHandler(Arc<AtomicBool>, Arc<AtomicBool>);
        impl crate::server::Handle for HangingHandler {
            async fn handle(
                &self,
                _: RequestHeaders,
                _: CallOptions,
                _: &mut impl SendStream,
                _: impl RecvStream + 'static,
            ) -> Trailers {
                self.0.store(true, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                self.1.store(true, Ordering::SeqCst);
                Trailers::new(Ok(()))
            }
        }

        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (resp_tx, _resp_rx) = mpsc::unbounded_channel();
        let (trailer_tx, _trailer_rx) = oneshot::channel();

        let transport = InMemoryServerCall {
            headers: RequestHeaders::new("", test_connection_info()),
            req_rx,
            resp_tx,
            trailer_tx,
        };

        let mut serving_conn = transport.serve(
            Arc::new(HangingHandler(hs, hf)),
            crate::rt::default_runtime(),
            crate::private::Internal,
        );

        // Poll the serving connection briefly to start the handler.
        tokio::select! {
            _ = &mut serving_conn => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        assert!(handler_started.load(Ordering::SeqCst));

        // Now DROP `serving_conn` (simulating what watch() does on `break`).
        drop(serving_conn);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // With the inline fix, dropping the connection future drops the handler future immediately -> handler never finishes.
        assert!(!handler_finished.load(Ordering::SeqCst));
    }
}
