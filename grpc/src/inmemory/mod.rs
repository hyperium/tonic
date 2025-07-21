use std::{
    collections::HashMap,
    ops::Add,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use crate::{
    client::{
        name_resolution::{
            self, global_registry, Address, ChannelController, Endpoint, Resolver, ResolverBuilder,
            ResolverOptions, ResolverUpdate,
        },
        transport::{self, ConnectedTransport, GLOBAL_TRANSPORT_REGISTRY},
    },
    server,
    service::{Request, Response, Service},
};
use once_cell::sync::Lazy;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tonic::async_trait;

pub struct Listener {
    id: String,
    s: Box<mpsc::Sender<Option<server::Call>>>,
    r: Arc<Mutex<mpsc::Receiver<Option<server::Call>>>>,
    // List of notifiers to call when closed.
    closed: Notify,
}

static ID: AtomicU32 = AtomicU32::new(0);

impl Listener {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel(1);
        let s = Arc::new(Self {
            id: format!("{}", ID.fetch_add(1, Ordering::Relaxed)),
            s: Box::new(tx),
            r: Arc::new(Mutex::new(rx)),
            closed: Notify::new(),
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
        self.closed.notify_waiters();
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
impl ConnectedTransport for Arc<Listener> {
    async fn disconnected(&self) {
        self.closed.notified().await;
    }
}

#[async_trait]
impl crate::server::Listener for Arc<Listener> {
    async fn accept(&self) -> Option<server::Call> {
        let mut recv = self.r.lock().await;
        let r = recv.recv().await;
        if r.is_none() {
            // Listener was closed.
            return None;
        }
        r.unwrap()
    }
}

static LISTENERS: Lazy<std::sync::Mutex<HashMap<String, Arc<Listener>>>> =
    Lazy::new(std::sync::Mutex::default);

struct ClientTransport {}

impl ClientTransport {
    fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl transport::Transport for ClientTransport {
    async fn connect(&self, address: String) -> Result<Box<dyn ConnectedTransport>, String> {
        let lis = LISTENERS
            .lock()
            .unwrap()
            .get(&address)
            .ok_or(format!("Could not find listener for address {address}"))?
            .clone();
        Ok(Box::new(lis))
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
