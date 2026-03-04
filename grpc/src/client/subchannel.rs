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

use core::panic;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::Notify;
use tokio::sync::oneshot;
use tonic::async_trait;

use crate::client::CallOptions;
use crate::client::ConnectivityState;
use crate::client::DynInvoke;
use crate::client::DynRecvStream;
use crate::client::DynSendStream;
use crate::client::channel::InternalChannelController;
use crate::client::channel::WorkQueueItem;
use crate::client::channel::WorkQueueTx;
use crate::client::load_balancing::ExternalSubchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::name_resolution::Address;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::core::RequestHeaders;
use crate::rt::GrpcRuntime;
use crate::service::Request;
use crate::service::Response;
use crate::service::Service;

// A temporary trait comprised of the old Service trait and the new DynInvoke
// trait.
pub(crate) trait SharedServiceTrait: DynInvoke + Service {}

type SharedService = Arc<dyn SharedServiceTrait>;

impl<T: DynInvoke + Service> SharedServiceTrait for T {}

pub trait Backoff: Send + Sync {
    fn backoff_until(&self) -> Instant;
    fn reset(&self);
    fn min_connect_timeout(&self) -> Duration;
}

// TODO(easwars): Move this somewhere else, where appropriate.
pub(crate) struct NopBackoff {}
impl Backoff for NopBackoff {
    fn backoff_until(&self) -> Instant {
        Instant::now()
    }
    fn reset(&self) {}
    fn min_connect_timeout(&self) -> Duration {
        Duration::from_secs(20)
    }
}

enum InternalSubchannelState {
    Idle,
    Connecting,
    Ready(SharedService),
    TransientFailure(String),
}

impl<'a> From<&'a InternalSubchannelState> for SubchannelState {
    fn from(iss: &'a InternalSubchannelState) -> SubchannelState {
        match &iss {
            InternalSubchannelState::Idle => SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            InternalSubchannelState::Connecting => SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                last_connection_error: None,
            },
            InternalSubchannelState::Ready(_) => SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            InternalSubchannelState::TransientFailure(err) => {
                let arc_err: Arc<dyn Error + Send + Sync> = Arc::from(Box::from(err.clone()));
                SubchannelState {
                    connectivity_state: ConnectivityState::TransientFailure,
                    last_connection_error: Some(arc_err),
                }
            }
        }
    }
}

impl Display for InternalSubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::TransientFailure(_) => write!(f, "TransientFailure"),
        }
    }
}

impl Debug for InternalSubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::TransientFailure(_) => write!(f, "TransientFailure"),
        }
    }
}

impl PartialEq for InternalSubchannelState {
    fn eq(&self, other: &Self) -> bool {
        match &self {
            Self::Idle => {
                if let Self::Idle = other {
                    return true;
                }
            }
            Self::Connecting => {
                if let Self::Connecting = other {
                    return true;
                }
            }
            Self::Ready(_) => {
                if let Self::Ready(_) = other {
                    return true;
                }
            }
            Self::TransientFailure(_) => {
                if let Self::TransientFailure(_) = other {
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl Service for InternalSubchannel {
    async fn call(&self, method: String, request: Request) -> Response {
        let svc = match &self.inner.inner.lock().unwrap().state {
            InternalSubchannelState::Ready(s) => s.clone(),
            _ => todo!("handle non-READY subchannel"),
        };

        return svc.call(method, request).await;
    }
}

#[async_trait]
impl DynInvoke for InternalSubchannel {
    async fn dyn_invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
        let svc = match &self.inner.inner.lock().unwrap().state {
            InternalSubchannelState::Ready(s) => s.clone(),
            _ => todo!("handle non-READY subchannel"),
        };
        svc.dyn_invoke(headers, options).await
    }
}

pub(crate) struct InternalSubchannel {
    unregister_fn: Option<Box<dyn FnOnce(SubchannelKey) + Send + Sync>>,
    inner: InnerSubchannel,
}

#[derive(Clone)]
struct InnerSubchannel {
    inner: Arc<Mutex<InnerSubchannelState>>,
}

struct InnerSubchannelState {
    key: SubchannelKey,
    state: InternalSubchannelState,
    watchers: Vec<Arc<SubchannelStateWatcher>>, // TODO(easwars): Revisit the choice for this data structure.
    on_drop: Arc<Notify>,
    transport_builder: Arc<dyn Transport>,
    backoff: Arc<dyn Backoff>,
    runtime: GrpcRuntime,
}

impl InnerSubchannelState {
    fn update_state(&mut self, state: InternalSubchannelState) {
        self.state = state;
        let state: SubchannelState = (&self.state).into();
        for w in &self.watchers {
            w.on_state_change(state.clone());
        }
    }
}

impl InternalSubchannel {
    pub(super) fn new(
        key: SubchannelKey,
        transport: Arc<dyn Transport>,
        backoff: Arc<dyn Backoff>,
        unregister_fn: Box<dyn FnOnce(SubchannelKey) + Send + Sync>,
        runtime: GrpcRuntime,
    ) -> Arc<InternalSubchannel> {
        println!("creating new internal subchannel for: {:?}", &key);
        Arc::new(Self {
            unregister_fn: Some(unregister_fn),
            inner: InnerSubchannel {
                inner: Arc::new(Mutex::new(InnerSubchannelState {
                    key,
                    transport_builder: transport,
                    backoff,
                    runtime,
                    state: InternalSubchannelState::Idle,
                    watchers: Vec::new(),
                    on_drop: Arc::default(),
                })),
            },
        })
    }

    pub(super) fn address(&self) -> Address {
        self.inner.inner.lock().unwrap().key.address.clone()
    }

    /// Begins connecting the subchannel asynchronously.  If now is set, does
    /// not wait for any pending connection backoff to complete.
    pub(super) fn connect(self: &Arc<Self>, now: bool) {
        if now || self.inner.inner.lock().unwrap().state == InternalSubchannelState::Idle {
            self.inner.move_to_connecting();
        }
        // TODO: latch connect request if !now && !Idle so subchannel
        // effectively skips idle and goes back to connecting immediatley.
    }

    pub(super) fn register_connectivity_state_watcher(&self, watcher: Arc<SubchannelStateWatcher>) {
        let mut inner = self.inner.inner.lock().unwrap();
        inner.watchers.push(watcher.clone());
        let state = (&inner.state).into();
        watcher.on_state_change(state);
    }

    pub(super) fn unregister_connectivity_state_watcher(
        &self,
        watcher: Arc<SubchannelStateWatcher>,
    ) {
        self.inner
            .inner
            .lock()
            .unwrap()
            .watchers
            .retain(|x| !Arc::ptr_eq(x, &watcher));
    }
}

impl InnerSubchannel {
    fn move_to_idle(&self) {
        self.inner
            .lock()
            .unwrap()
            .update_state(InternalSubchannelState::Idle);
    }

    fn move_to_connecting(&self) {
        // TODO: All these options to be configured by users.
        let transport_opts = TransportOptions::default();

        let mut state = self.inner.lock().unwrap();
        let self_clone = self.clone();
        let backoff = state.backoff.min_connect_timeout();
        let transport_builder = state.transport_builder.clone();
        let address = state.key.address.address.to_string();
        let runtime = state.runtime.clone();
        let on_drop = state.on_drop.clone();
        state.runtime.spawn(Box::pin(async move {
            tokio::select! {
                _ = runtime.sleep(backoff) => {
                    self_clone.move_to_transient_failure("connect timeout expired".into());
                }
                _ = on_drop.notified() => {
                }
                result = transport_builder.connect(address, runtime, &transport_opts) => {
                    match result {
                        Ok(s) => {
                            self_clone.move_to_ready(Arc::from(s.service), s.disconnection_listener);
                        }
                        Err(e) => {
                            self_clone.move_to_transient_failure(e);
                        }
                    }
                },
            }
        }));
        state.update_state(InternalSubchannelState::Connecting);
    }

    fn move_to_ready(&self, svc: SharedService, closed_rx: oneshot::Receiver<Result<(), String>>) {
        let mut state = self.inner.lock().unwrap();
        // Reset connection backoff upon successfully moving to ready.
        state.backoff.reset();
        let self_clone = self.clone();
        let on_drop = state.on_drop.clone();
        state.runtime.spawn(Box::pin(async move {
            // TODO(easwars): Does it make sense for disconnected() to return an
            // error string containing information about why the connection
            // terminated? But what can we do with that error other than logging
            // it, which the transport can do as well?
            tokio::select! {
                _ = on_drop.notified() => {}
                e = closed_rx => {
                    eprintln!("Transport closed: {e:?}");
                    self_clone.move_to_idle();
                }
            }
        }));
        state.update_state(InternalSubchannelState::Ready(svc.clone()));
    }

    fn move_to_transient_failure(&self, err: String) {
        let mut state = self.inner.lock().unwrap();
        let backoff_interval = state.backoff.backoff_until();
        let runtime = state.runtime.clone();
        let on_drop = state.on_drop.clone();
        let self_clone = self.clone();
        state.runtime.spawn(Box::pin(async move {
            tokio::select! {
                _ = on_drop.notified() => {}
                _ = runtime.sleep(backoff_interval.saturating_duration_since(Instant::now())) => {
                    self_clone.move_to_idle();
                }
            }
        }));
        state.update_state(InternalSubchannelState::TransientFailure(err.clone()));
    }
}

impl Drop for InternalSubchannel {
    fn drop(&mut self) {
        let key;
        let on_drop;
        {
            let state = self.inner.inner.lock().unwrap();
            key = state.key.clone();
            on_drop = state.on_drop.clone();
        }

        let unregister_fn = self.unregister_fn.take();
        unregister_fn.unwrap()(key);
        on_drop.notify_waiters();
    }
}

// SubchannelKey uniiquely identifies a subchannel in the pool.
#[derive(PartialEq, PartialOrd, Eq, Ord, Clone)]

pub(crate) struct SubchannelKey {
    address: Address,
}

impl SubchannelKey {
    pub(crate) fn new(address: Address) -> Self {
        Self { address }
    }
}

impl Display for SubchannelKey {
    #[allow(clippy::to_string_in_format_args)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.address.address.to_string())
    }
}

impl Debug for SubchannelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.address)
    }
}

pub(super) struct InternalSubchannelPool {
    subchannels: RwLock<BTreeMap<SubchannelKey, Weak<InternalSubchannel>>>,
}

impl InternalSubchannelPool {
    pub(super) fn new() -> Self {
        Self {
            subchannels: RwLock::new(BTreeMap::new()),
        }
    }

    pub(super) fn lookup_subchannel(&self, key: &SubchannelKey) -> Option<Arc<InternalSubchannel>> {
        println!("looking up subchannel for: {key:?} in the pool");
        if let Some(weak_isc) = self.subchannels.read().unwrap().get(key)
            && let Some(isc) = weak_isc.upgrade()
        {
            return Some(isc);
        }
        None
    }

    pub(super) fn register_subchannel(
        &self,
        key: &SubchannelKey,
        isc: Arc<InternalSubchannel>,
    ) -> Arc<InternalSubchannel> {
        println!("registering subchannel for: {key:?} with the pool");
        self.subchannels
            .write()
            .unwrap()
            .insert(key.clone(), Arc::downgrade(&isc));
        isc
    }

    pub(super) fn unregister_subchannel(&self, key: &SubchannelKey) {
        let mut subchannels = self.subchannels.write().unwrap();
        if let Some(weak_isc) = subchannels.get(key) {
            if let Some(isc) = weak_isc.upgrade() {
                return;
            }
            println!("removing subchannel for: {key:?} from the pool");
            subchannels.remove(key);
            return;
        }
        panic!("attempt to unregister subchannel for unknown key {:?}", key);
    }
}

#[derive(Clone)]
pub(super) struct SubchannelStateWatcher {
    subchannel: Weak<ExternalSubchannel>,
    work_scheduler: WorkQueueTx,
}

impl SubchannelStateWatcher {
    pub(super) fn new(sc: Arc<ExternalSubchannel>, work_scheduler: WorkQueueTx) -> Self {
        Self {
            subchannel: Arc::downgrade(&sc),
            work_scheduler,
        }
    }

    fn on_state_change(&self, state: SubchannelState) {
        // Ignore internal subchannel state changes if the external subchannel
        // was dropped but its state watcher is still pending unregistration;
        // such updates are inconsequential.
        if let Some(sc) = self.subchannel.upgrade() {
            let _ = self.work_scheduler.send(WorkQueueItem::Closure(Box::new(
                move |c: &mut InternalChannelController| {
                    c.lb.clone()
                        .policy
                        .lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .subchannel_update(sc, &state, c);
                },
            )));
        }
    }
}
