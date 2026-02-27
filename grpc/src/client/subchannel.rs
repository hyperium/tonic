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

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tonic::async_trait;

use crate::client::ConnectivityState;
use crate::client::channel::InternalChannelController;
use crate::client::channel::WorkQueueItem;
use crate::client::channel::WorkQueueTx;
use crate::client::load_balancing::ExternalSubchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::name_resolution::Address;
use crate::client::transport::Transport;
use crate::client::transport::TransportOptions;
use crate::rt::BoxedTaskHandle;
use crate::rt::GrpcRuntime;
use crate::service::Request;
use crate::service::Response;
use crate::service::Service;

type SharedService = Arc<dyn Service>;

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
    Connecting(InternalSubchannelConnectingState),
    Ready(InternalSubchannelReadyState),
    TransientFailure(InternalSubchannelTransientFailureState),
}

struct InternalSubchannelConnectingState {
    abort_handle: Option<BoxedTaskHandle>,
}

struct InternalSubchannelReadyState {
    abort_handle: Option<BoxedTaskHandle>,
    svc: SharedService,
}

struct InternalSubchannelTransientFailureState {
    task_handle: Option<BoxedTaskHandle>,
    error: String,
}

impl InternalSubchannelState {
    fn connected_transport(&self) -> Option<SharedService> {
        match self {
            Self::Ready(st) => Some(st.svc.clone()),
            _ => None,
        }
    }

    fn to_subchannel_state(&self) -> SubchannelState {
        match self {
            Self::Idle => SubchannelState {
                connectivity_state: ConnectivityState::Idle,
                last_connection_error: None,
            },
            Self::Connecting(_) => SubchannelState {
                connectivity_state: ConnectivityState::Connecting,
                last_connection_error: None,
            },
            Self::Ready(_) => SubchannelState {
                connectivity_state: ConnectivityState::Ready,
                last_connection_error: None,
            },
            Self::TransientFailure(st) => {
                let arc_err: Arc<dyn Error + Send + Sync> = Arc::from(Box::from(st.error.clone()));
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
            Self::Connecting(_) => write!(f, "Connecting"),
            Self::Ready(_) => write!(f, "Ready"),
            Self::TransientFailure(_) => write!(f, "TransientFailure"),
        }
    }
}

impl Debug for InternalSubchannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Connecting(_) => write!(f, "Connecting"),
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
            Self::Connecting(_) => {
                if let Self::Connecting(_) = other {
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

impl Drop for InternalSubchannelState {
    fn drop(&mut self) {
        match &self {
            Self::Idle => {}
            Self::Connecting(st) => {
                if let Some(ah) = &st.abort_handle {
                    ah.abort();
                }
            }
            Self::Ready(st) => {
                if let Some(ah) = &st.abort_handle {
                    ah.abort();
                }
            }
            Self::TransientFailure(st) => {
                if let Some(ah) = &st.task_handle {
                    ah.abort();
                }
            }
        }
    }
}

pub(crate) struct InternalSubchannel {
    key: SubchannelKey,
    transport: Arc<dyn Transport>,
    backoff: Arc<dyn Backoff>,
    unregister_fn: Option<Box<dyn FnOnce(SubchannelKey) + Send + Sync>>,
    state_machine_event_sender: mpsc::UnboundedSender<SubchannelStateMachineEvent>,
    inner: Mutex<InnerSubchannel>,
    runtime: GrpcRuntime,
}

struct InnerSubchannel {
    state: InternalSubchannelState,
    watchers: Vec<Arc<SubchannelStateWatcher>>, // TODO(easwars): Revisit the choice for this data structure.
    backoff_task: Option<BoxedTaskHandle>,
    disconnect_task: Option<BoxedTaskHandle>,
}

#[async_trait]
impl Service for InternalSubchannel {
    async fn call(&self, method: String, request: Request) -> Response {
        let svc = self.inner.lock().unwrap().state.connected_transport();
        if svc.is_none() {
            // TODO(easwars): Change the signature of this method to return a
            // Result<Response, Error>
            panic!("todo: handle !ready");
        }

        let svc = svc.unwrap().clone();
        return svc.call(method, request).await;
    }
}

enum SubchannelStateMachineEvent {
    ConnectionRequested,
    ConnectionSucceeded(SharedService, oneshot::Receiver<Result<(), String>>),
    ConnectionTimedOut,
    ConnectionFailed(String),
    ConnectionTerminated,
    BackoffExpired,
}
impl Debug for SubchannelStateMachineEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionRequested => write!(f, "ConnectionRequested"),
            Self::ConnectionSucceeded(_, _) => write!(f, "ConnectionSucceeded"),
            Self::ConnectionTimedOut => write!(f, "ConnectionTimedOut"),
            Self::ConnectionFailed(_) => write!(f, "ConnectionFailed"),
            Self::ConnectionTerminated => write!(f, "ConnectionTerminated"),
            Self::BackoffExpired => write!(f, "BackoffExpired"),
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
        let (tx, mut rx) = mpsc::unbounded_channel::<SubchannelStateMachineEvent>();
        let isc = Arc::new(Self {
            key: key.clone(),
            transport,
            backoff: backoff.clone(),
            unregister_fn: Some(unregister_fn),
            state_machine_event_sender: tx,
            inner: Mutex::new(InnerSubchannel {
                state: InternalSubchannelState::Idle,
                watchers: Vec::new(),
                backoff_task: None,
                disconnect_task: None,
            }),
            runtime: runtime.clone(),
        });

        // This long running task implements the subchannel state machine. When
        // the subchannel is dropped, the channel from which this task reads is
        // closed, and therefore this task exits because rx.recv() returns None
        // in that case.
        let arc_to_self = Arc::clone(&isc);
        runtime.spawn(Box::pin(async move {
            println!("starting subchannel state machine for: {:?}", &key);
            while let Some(m) = rx.recv().await {
                println!("subchannel {:?} received event {:?}", &key, &m);
                match m {
                    SubchannelStateMachineEvent::ConnectionRequested => {
                        arc_to_self.move_to_connecting();
                    }
                    SubchannelStateMachineEvent::ConnectionSucceeded(svc, rx) => {
                        arc_to_self.move_to_ready(svc, rx);
                    }
                    SubchannelStateMachineEvent::ConnectionTimedOut => {
                        arc_to_self.move_to_transient_failure("connect timeout expired".into());
                    }
                    SubchannelStateMachineEvent::ConnectionFailed(err) => {
                        arc_to_self.move_to_transient_failure(err);
                    }
                    SubchannelStateMachineEvent::ConnectionTerminated => {
                        arc_to_self.move_to_idle();
                    }
                    SubchannelStateMachineEvent::BackoffExpired => {
                        arc_to_self.move_to_idle();
                    }
                }
            }
            println!("exiting work queue task in subchannel");
        }));
        isc
    }

    pub(super) fn address(&self) -> Address {
        self.key.address.clone()
    }

    /// Begins connecting the subchannel asynchronously.  If now is set, does
    /// not wait for any pending connection backoff to complete.
    pub(super) fn connect(&self, now: bool) {
        let state = &self.inner.lock().unwrap().state;
        if let InternalSubchannelState::Idle = state {
            let _ = self
                .state_machine_event_sender
                .send(SubchannelStateMachineEvent::ConnectionRequested);
        }
    }

    pub(super) fn register_connectivity_state_watcher(&self, watcher: Arc<SubchannelStateWatcher>) {
        let mut inner = self.inner.lock().unwrap();
        inner.watchers.push(watcher.clone());
        let state = inner.state.to_subchannel_state().clone();
        watcher.on_state_change(state);
    }

    pub(super) fn unregister_connectivity_state_watcher(
        &self,
        watcher: Arc<SubchannelStateWatcher>,
    ) {
        self.inner
            .lock()
            .unwrap()
            .watchers
            .retain(|x| !Arc::ptr_eq(x, &watcher));
    }

    fn notify_watchers(&self, state: SubchannelState) {
        let mut inner = self.inner.lock().unwrap();
        inner.state = InternalSubchannelState::Idle;
        for w in &inner.watchers {
            w.on_state_change(state.clone());
        }
    }

    fn move_to_idle(&self) {
        self.notify_watchers(SubchannelState {
            connectivity_state: ConnectivityState::Idle,
            last_connection_error: None,
        });
    }

    fn move_to_connecting(&self) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.state = InternalSubchannelState::Connecting(InternalSubchannelConnectingState {
                abort_handle: None,
            });
        }
        self.notify_watchers(SubchannelState {
            connectivity_state: ConnectivityState::Connecting,
            last_connection_error: None,
        });

        let min_connect_timeout = self.backoff.min_connect_timeout();
        let transport = self.transport.clone();
        let address = self.address().address;
        let state_machine_tx = self.state_machine_event_sender.clone();
        // TODO: All these options to be configured by users.
        let transport_opts = TransportOptions::default();
        let runtime = self.runtime.clone();

        let connect_task = self.runtime.spawn(Box::pin(async move {
            tokio::select! {
                _ = runtime.sleep(min_connect_timeout) => {
                    let _ = state_machine_tx.send(SubchannelStateMachineEvent::ConnectionTimedOut);
                }
                result = transport.connect(address.to_string().clone(), runtime, &transport_opts) => {
                    match result {
                        Ok(s) => {
                            let _ = state_machine_tx.send(SubchannelStateMachineEvent::ConnectionSucceeded(Arc::from(s.service), s.disconnection_listener));
                        }
                        Err(e) => {
                            let _ = state_machine_tx.send(SubchannelStateMachineEvent::ConnectionFailed(e));
                        }
                    }
                },
            }
        }));
        let mut inner = self.inner.lock().unwrap();
        inner.state = InternalSubchannelState::Connecting(InternalSubchannelConnectingState {
            abort_handle: Some(connect_task),
        });
    }

    fn move_to_ready(&self, svc: SharedService, closed_rx: oneshot::Receiver<Result<(), String>>) {
        let svc2 = svc.clone();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.state = InternalSubchannelState::Ready(InternalSubchannelReadyState {
                abort_handle: None,
                svc: svc2.clone(),
            });
        }
        self.notify_watchers(SubchannelState {
            connectivity_state: ConnectivityState::Ready,
            last_connection_error: None,
        });

        let state_machine_tx = self.state_machine_event_sender.clone();
        let task_handle = self.runtime.spawn(Box::pin(async move {
            // TODO(easwars): Does it make sense for disconnected() to return an
            // error string containing information about why the connection
            // terminated? But what can we do with that error other than logging
            // it, which the transport can do as well?
            if let Err(e) = closed_rx.await {
                eprintln!("Transport closed with error: {e}",)
            };
            let _ = state_machine_tx.send(SubchannelStateMachineEvent::ConnectionTerminated);
        }));
        let mut inner = self.inner.lock().unwrap();
        inner.state = InternalSubchannelState::Ready(InternalSubchannelReadyState {
            abort_handle: Some(task_handle),
            svc: svc2.clone(),
        });
    }

    fn move_to_transient_failure(&self, err: String) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.state = InternalSubchannelState::TransientFailure(
                InternalSubchannelTransientFailureState {
                    task_handle: None,
                    error: err.clone(),
                },
            );
        }

        let arc_err: Arc<dyn Error + Send + Sync> = Arc::from(Box::from(err.clone()));
        self.notify_watchers(SubchannelState {
            connectivity_state: ConnectivityState::TransientFailure,
            last_connection_error: Some(arc_err.clone()),
        });

        let backoff_interval = self.backoff.backoff_until();
        let state_machine_tx = self.state_machine_event_sender.clone();
        let runtime = self.runtime.clone();
        let backoff_task = self.runtime.spawn(Box::pin(async move {
            runtime
                .sleep(backoff_interval.saturating_duration_since(Instant::now()))
                .await;
            let _ = state_machine_tx.send(SubchannelStateMachineEvent::BackoffExpired);
        }));
        let mut inner = self.inner.lock().unwrap();
        inner.state =
            InternalSubchannelState::TransientFailure(InternalSubchannelTransientFailureState {
                task_handle: Some(backoff_task),
                error: err.clone(),
            });
    }

    /// Wait for any in-flight RPCs to terminate and then close the connection
    /// and destroy the Subchannel.
    async fn drain(self) {}
}

impl Drop for InternalSubchannel {
    fn drop(&mut self) {
        println!("dropping internal subchannel {:?}", self.key);
        let unregister_fn = self.unregister_fn.take();
        unregister_fn.unwrap()(self.key.clone());
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
        if let Some(weak_isc) = self.subchannels.read().unwrap().get(key) {
            if let Some(isc) = weak_isc.upgrade() {
                return Some(isc);
            }
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
