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
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;
use tonic::async_trait;

use crate::client::name_resolution::global_registry;
use crate::client::name_resolution::Address;
use crate::client::name_resolution::ChannelController;
use crate::client::name_resolution::Endpoint;
use crate::client::name_resolution::Resolver;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverOptions;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::{self};
use crate::client::transport::ConnectedTransport;
use crate::client::transport::TransportOptions;
use crate::client::transport::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::{self};
use crate::rt::GrpcRuntime;
use crate::server;
use crate::service::Request;
use crate::service::Response;
use crate::service::Service;

pub struct Listener {
    id: String,
    s: Box<mpsc::Sender<Option<server::Call>>>,
    r: Arc<AsyncMutex<mpsc::Receiver<Option<server::Call>>>>,
    // List of notifiers to call when closed.
    #[allow(clippy::type_complexity)]
    closed_tx: Arc<Mutex<Vec<oneshot::Sender<Result<(), String>>>>>,
}

static ID: AtomicU32 = AtomicU32::new(0);

impl Listener {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel(1);
        let s = Arc::new(Self {
            id: format!("{}", ID.fetch_add(1, Ordering::Relaxed)),
            s: Box::new(tx),
            r: Arc::new(AsyncMutex::new(rx)),
            closed_tx: Arc::new(Mutex::new(Vec::new())),
        });
        LISTENERS.lock().unwrap().insert(s.id.clone(), s.clone());
        s
    }

    pub fn target(&self) -> String {
        format!("inmemory:///{}", self.id)
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    pub async fn close(&self) {
        let _ = self.s.send(None).await;
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let txs = std::mem::take(&mut *self.closed_tx.lock().unwrap());
        for rx in txs {
            let _ = rx.send(Ok(()));
        }
        LISTENERS.lock().unwrap().remove(&self.id);
    }
}

#[async_trait]
impl Service for Arc<Listener> {
    async fn call(&self, method: String, request: Request) -> Response {
        // 1. unblock accept, giving it a func back to me
        // 2. return what that func had
        let (s, r) = oneshot::channel();
        self.s.send(Some((method, request, s))).await.unwrap();
        r.await.unwrap()
    }
}

#[async_trait]
impl crate::server::Listener for Arc<Listener> {
    async fn accept(&self) -> Option<server::Call> {
        let mut recv = self.r.lock().await;
        let r = recv.recv().await;
        // Listener may be closed.
        r?
    }
}

static LISTENERS: LazyLock<Mutex<HashMap<String, Arc<Listener>>>> = LazyLock::new(Mutex::default);

struct ClientTransport {}

impl ClientTransport {
    fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl transport::Transport for ClientTransport {
    async fn connect(
        &self,
        address: String,
        _: GrpcRuntime,
        _: &TransportOptions,
    ) -> Result<ConnectedTransport, String> {
        let lis = LISTENERS
            .lock()
            .unwrap()
            .get(&address)
            .ok_or(format!("Could not find listener for address {address}"))?
            .clone();
        let (tx, rx) = oneshot::channel();
        lis.closed_tx.lock().unwrap().push(tx);
        Ok(ConnectedTransport {
            service: Box::new(lis),
            disconnection_listener: rx,
        })
    }
}

static INMEMORY_NETWORK_TYPE: &str = "inmemory";

pub fn reg() {
    GLOBAL_TRANSPORT_REGISTRY.add_transport(INMEMORY_NETWORK_TYPE, ClientTransport::new());
    global_registry().add_builder(Box::new(InMemoryResolverBuilder));
}

struct InMemoryResolverBuilder;

impl ResolverBuilder for InMemoryResolverBuilder {
    fn scheme(&self) -> &'static str {
        "inmemory"
    }

    fn build(
        &self,
        target: &name_resolution::Target,
        options: ResolverOptions,
    ) -> Box<dyn Resolver> {
        let id = target.path().strip_prefix("/").unwrap().to_string();
        options.work_scheduler.schedule_work();
        Box::new(NopResolver { id })
    }

    fn is_valid_uri(&self, uri: &crate::client::name_resolution::Target) -> bool {
        true
    }
}

struct NopResolver {
    id: String,
}

impl Resolver for NopResolver {
    fn work(&mut self, channel_controller: &mut dyn ChannelController) {
        let mut addresses: Vec<Address> = Vec::new();
        for addr in LISTENERS.lock().unwrap().keys() {
            addresses.push(Address {
                network_type: INMEMORY_NETWORK_TYPE,
                address: addr.clone().into(),
                ..Default::default()
            });
        }

        let _ = channel_controller.update(ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses,
                ..Default::default()
            }]),
            ..Default::default()
        });
    }

    fn resolve_now(&mut self) {}
}
