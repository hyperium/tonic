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
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use bytes::Buf;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::client::CallOptions;
use crate::client::DynRecvStream as ClientDynRecvStream;
use crate::client::DynSendStream as ClientDynSendStream;
use crate::client::Invoke;
use crate::client::RecvStream as ClientRecvStream;
use crate::client::SendOptions as ClientSendOptions;
use crate::client::SendStream as ClientSendStream;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::ChannelController as ResolverChannelController;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::Resolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverOptions;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::global_registry as global_resolver_registry;
use crate::client::service_config::ServiceConfig;
use crate::client::transport::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::ResponseHeaders;
use crate::core::ResponseStreamItem;
use crate::core::SendMessage;
use crate::core::Trailers;
use crate::rt::GrpcRuntime;
use crate::server::Call as ServerCall;
use crate::server::Listener as ServerListener;
use crate::server::RecvStream as ServerRecvStream;
use crate::server::SendOptions as ServerSendOptions;
use crate::server::SendStream as ServerSendStream;

static LISTENERS: LazyLock<Mutex<HashMap<String, mpsc::Sender<InMemoryServerCall>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct InMemoryServerCall {
    pub headers: RequestHeaders,
    pub req_rx: mpsc::UnboundedReceiver<InMemoryRequestStreamItem>,
    pub resp_tx: mpsc::UnboundedSender<InMemoryResponseStreamItem>,
}

pub enum InMemoryRequestStreamItem {
    Message(Box<dyn Buf + Send + Sync>),
    StreamClosed,
}

pub enum InMemoryResponseStreamItem {
    Headers(ResponseHeaders),
    Message(Box<dyn Buf + Send + Sync>),
    Trailers(Trailers),
    StreamClosed,
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

impl ServerListener for InMemoryListener {
    type SendStream = InMemoryServerSendStream;
    type RecvStream = InMemoryServerRecvStream;

    async fn accept(&self) -> Option<ServerCall<Self::SendStream, Self::RecvStream>> {
        let mut r = self.inner.r.lock().await;
        tokio::select! {
            call = r.recv() => {
                let call = call?;
                Some(ServerCall {
                    headers: call.headers,
                    send: InMemoryServerSendStream { tx: call.resp_tx },
                    recv: InMemoryServerRecvStream { rx: call.req_rx },
                })
            }
            _ = self.inner.close_notify.notified() => {
                None
            }
        }
    }
}

pub struct InMemoryServerSendStream {
    tx: mpsc::UnboundedSender<InMemoryResponseStreamItem>,
}

impl ServerSendStream for InMemoryServerSendStream {
    async fn send<'a>(
        &mut self,
        item: crate::core::ServerResponseStreamItem<'a>,
        _options: ServerSendOptions,
    ) -> Result<(), ()> {
        let inmemory_item = match item {
            ResponseStreamItem::Headers(h) => InMemoryResponseStreamItem::Headers(h),
            ResponseStreamItem::Message(m) => {
                let buf = m.encode().map_err(|_| ())?;
                InMemoryResponseStreamItem::Message(buf)
            }
            ResponseStreamItem::Trailers(t) => InMemoryResponseStreamItem::Trailers(t),
            ResponseStreamItem::StreamClosed => InMemoryResponseStreamItem::StreamClosed,
        };

        self.tx.send(inmemory_item).map_err(|_| ())
    }
}

pub struct InMemoryServerRecvStream {
    rx: mpsc::UnboundedReceiver<InMemoryRequestStreamItem>,
}

impl ServerRecvStream for InMemoryServerRecvStream {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> Result<(), ()> {
        match self.rx.recv().await {
            Some(InMemoryRequestStreamItem::Message(mut buf)) => {
                msg.decode(&mut buf).map_err(|_| ())
            }
            _ => Err(()),
        }
    }
}

pub struct InMemoryConnection {
    s: mpsc::Sender<InMemoryServerCall>,
    closed_tx: Option<oneshot::Sender<Result<(), String>>>,
}

impl Invoke for InMemoryConnection {
    type SendStream = Box<dyn ClientDynSendStream>;
    type RecvStream = Box<dyn ClientDynRecvStream>;

    async fn invoke(
        &self,
        headers: RequestHeaders,
        _options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        let (req_tx, req_rx) = mpsc::unbounded_channel::<InMemoryRequestStreamItem>();
        let (resp_tx, resp_rx) = mpsc::unbounded_channel::<InMemoryResponseStreamItem>();

        let call = InMemoryServerCall {
            headers,
            req_rx,
            resp_tx,
        };

        let _ = self.s.try_send(call);

        (
            Box::new(InMemoryClientSendStream { tx: Some(req_tx) }),
            Box::new(InMemoryClientRecvStream { rx: resp_rx }),
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
}

impl ClientRecvStream for InMemoryClientRecvStream {
    async fn next(&mut self, msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
        match self.rx.recv().await {
            Some(InMemoryResponseStreamItem::Headers(h)) => ClientResponseStreamItem::Headers(h),
            Some(InMemoryResponseStreamItem::Message(mut buf)) => {
                msg.decode(&mut buf).unwrap();
                ClientResponseStreamItem::Message(())
            }
            Some(InMemoryResponseStreamItem::Trailers(t)) => ClientResponseStreamItem::Trailers(t),
            _ => ClientResponseStreamItem::StreamClosed,
        }
    }
}

pub struct InMemoryTransport {}

impl Transport for InMemoryTransport {
    type Service = InMemoryConnection;

    async fn connect(
        &self,
        target: String,
        _runtime: GrpcRuntime,
        _options: &TransportOptions,
    ) -> Result<(Self::Service, oneshot::Receiver<Result<(), String>>), String> {
        let listeners = LISTENERS.lock().unwrap();
        let s = listeners
            .get(&target)
            .ok_or_else(|| format!("no listener for target: {}", target))?;

        let (closed_tx, closed_rx) = oneshot::channel();
        let conn = InMemoryConnection {
            s: s.clone(),
            closed_tx: Some(closed_tx),
        };

        Ok((conn, closed_rx))
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

        let _ = channel_controller.update(ResolverUpdate {
            endpoints: Ok(endpoints),
            service_config: Ok(Some(ServiceConfig {
                load_balancing_policy: Some(
                    crate::client::service_config::LbPolicyType::RoundRobin,
                ),
            })),
            ..Default::default()
        });
    }
}

pub fn reg() {
    GLOBAL_TRANSPORT_REGISTRY.add_transport("inmemory", InMemoryTransport {});
    global_resolver_registry().add_builder(Box::new(InMemoryResolverBuilder {}));
}
