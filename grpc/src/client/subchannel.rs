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

use crate::Status;
use crate::StatusCode;
use crate::client::CallOptions;
use crate::client::ConnectivityState;
use crate::client::DynInvoke;
use crate::client::DynRecvStream;
use crate::client::DynSendStream;
use crate::client::RecvStream;
use crate::client::SendOptions;
use crate::client::SendStream;
use crate::client::channel::InternalChannelController;
use crate::client::channel::WorkQueueItem;
use crate::client::channel::WorkQueueTx;
use crate::client::load_balancing::ExternalSubchannel;
use crate::client::load_balancing::SubchannelState;
use crate::client::name_resolution::Address;
use crate::client::transport::DynTransport;
use crate::client::transport::SecurityOpts;
use crate::client::transport::TransportOptions;
use crate::core::ClientResponseStreamItem;
use crate::core::RecvMessage;
use crate::core::RequestHeaders;
use crate::core::SendMessage;
use crate::core::Trailers;
use crate::credentials::call::CallDetails;
use crate::credentials::call::ClientConnectionSecurityInfo as CallClientConnectionSecurityInfo;
use crate::credentials::client::ClientConnectionSecurityContext;
use crate::credentials::client::ClientConnectionSecurityInfo;
use crate::credentials::common::Authority;
use crate::rt::GrpcRuntime;

type SharedInvoke = Arc<dyn DynInvoke>;

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

struct ReadyState {
    service: Box<dyn DynInvoke>,
    security_info: ClientConnectionSecurityInfo<Box<dyn ClientConnectionSecurityContext>>,
    authority: Authority,
}

enum InternalSubchannelState {
    Idle,
    Connecting,
    Ready(Arc<ReadyState>),
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
impl DynInvoke for InternalSubchannel {
    async fn dyn_invoke(
        &self,
        mut headers: RequestHeaders,
        options: CallOptions,
    ) -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
        let (state, call_creds) = {
            let data = self.inner.data.lock().unwrap();

            let state = match &data.state {
                InternalSubchannelState::Ready(state) => state.clone(),
                _ => todo!("handle non-READY subchannel"),
            };

            let creds = data
                .security_opts
                .credentials
                .get_call_credentials()
                .cloned();

            (state, creds)
        };

        let fail_with = |status| -> (Box<dyn DynSendStream>, Box<dyn DynRecvStream>) {
            (
                Box::new(FailingSendStream {}),
                Box::new(FailingRecvStream::new(status)),
            )
        };

        if let Some(call_creds) = call_creds {
            if call_creds.minimum_channel_security_level() > state.security_info.security_level() {
                return fail_with(Status::new(
                    StatusCode::Unauthenticated,
                    "transport: cannot send secure credentials on an insecure connection",
                ));
            }

            let call_details = create_call_details(&state.authority, headers.method_name());

            let channel_sec_info = CallClientConnectionSecurityInfo::new(
                state.security_info.security_protocol(),
                state.security_info.security_level(),
                state.security_info.attributes().clone(),
            );

            if let Err(s) = call_creds
                .get_metadata(&call_details, &channel_sec_info, headers.metadata_mut())
                .await
            {
                let status = if s.is_restricted_control_plane_code() {
                    Status::new(
                        StatusCode::Internal,
                        format!(
                            "transport: received call credentials error with illegal status: {}",
                            s.message()
                        ),
                    )
                } else {
                    s
                };

                return fail_with(status);
            }
        }

        state.service.dyn_invoke(headers, options).await
    }
}

pub(crate) struct InternalSubchannel {
    unregister_fn: Option<Box<dyn FnOnce(SubchannelKey) + Send + Sync>>,
    key: SubchannelKey,
    inner: InnerSubchannel,
    on_drop: Arc<Notify>,
}

#[derive(Clone)]
struct InnerSubchannel {
    data: Arc<Mutex<SharedInnerSubchannelData>>,
}

struct SharedInnerSubchannelData {
    address: String,
    state: InternalSubchannelState,
    watchers: Vec<Arc<SubchannelStateWatcher>>, // TODO(easwars): Revisit the choice for this data structure.
    on_drop: Arc<Notify>,
    transport_builder: Arc<dyn DynTransport>,
    backoff: Arc<dyn Backoff>,
    runtime: GrpcRuntime,
    transport_options: TransportOptions,
    security_opts: SecurityOpts,
}

impl SharedInnerSubchannelData {
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
        transport: Arc<dyn DynTransport>,
        backoff: Arc<dyn Backoff>,
        unregister_fn: Box<dyn FnOnce(SubchannelKey) + Send + Sync>,
        runtime: GrpcRuntime,
        security_opts: SecurityOpts,
    ) -> Arc<InternalSubchannel> {
        println!("creating new internal subchannel for: {:?}", &key);
        let address = key.address.address.to_string();
        let on_drop = Arc::new(Notify::new());
        Arc::new(Self {
            key,
            on_drop: on_drop.clone(),
            unregister_fn: Some(unregister_fn),
            inner: InnerSubchannel {
                data: Arc::new(Mutex::new(SharedInnerSubchannelData {
                    address,
                    transport_builder: transport,
                    backoff,
                    runtime,
                    state: InternalSubchannelState::Idle,
                    watchers: Vec::new(),
                    on_drop,
                    transport_options: TransportOptions::default(), // TODO: should be configurable
                    security_opts,
                })),
            },
        })
    }

    pub(super) fn address(&self) -> Address {
        self.key.address.clone()
    }

    /// Begins connecting the subchannel asynchronously.  Does nothing if the
    /// subchannel is not currently idle.
    pub(super) fn connect(self: &Arc<Self>) {
        self.inner.begin_connecting();
    }

    pub(super) fn register_connectivity_state_watcher(&self, watcher: Arc<SubchannelStateWatcher>) {
        let mut data = self.inner.data.lock().unwrap();
        data.watchers.push(watcher.clone());
        let state = (&data.state).into();
        watcher.on_state_change(state);
    }

    pub(super) fn unregister_connectivity_state_watcher(
        &self,
        watcher: Arc<SubchannelStateWatcher>,
    ) {
        self.inner
            .data
            .lock()
            .unwrap()
            .watchers
            .retain(|x| !Arc::ptr_eq(x, &watcher));
    }
}

// The InnerSubchannel states progress as follows:
//
// Idle -> Connecting -> Ready -> Idle [after disconnect]
// or
// Idle -> Connecting -> TransientFailure -> Idle [after backoff]
//
// Idle is always a terminal state.
impl InnerSubchannel {
    fn move_to_idle(&self) {
        self.data
            .lock()
            .unwrap()
            .update_state(InternalSubchannelState::Idle);
    }

    // Starts connecting in the background and manages the full lifecycle of the
    // subchannel until it returns back to idle in that background task.
    fn begin_connecting(&self) {
        let mut data = self.data.lock().unwrap();
        if data.state != InternalSubchannelState::Idle {
            return;
        }
        data.update_state(InternalSubchannelState::Connecting);

        let self_clone = self.clone();
        let connect_timeout = data.backoff.min_connect_timeout();
        let transport_builder = data.transport_builder.clone();
        let address = data.address.clone();
        let runtime = data.runtime.clone();
        let on_drop = data.on_drop.clone();
        let transport_opts = data.transport_options.clone();
        let security_opts = data.security_opts.clone();
        data.runtime.spawn(Box::pin(async move {
            tokio::select! {
                _ = runtime.sleep(connect_timeout) => {
                    self_clone.move_to_transient_failure("connect timeout expired".into()).await;
                }
                _ = on_drop.notified() => {
                }
                result = transport_builder.dyn_connect(address, runtime, &security_opts, &transport_opts) => {
                    match result {
                        Ok((service, security_info, disconnection_listener)) => {
                            self_clone.move_to_ready(Arc::new(ReadyState{
                                service,
                                security_info,
                                authority: security_opts.authority}), disconnection_listener).await;
                        }
                        Err(e) => {
                            self_clone.move_to_transient_failure(e).await;
                        }
                    }
                },
            }
        }));
    }

    // Sets the state to ready and then waits until the subchannel is dropped or
    // the connection is lost.  Moves to idle upon connection loss.
    async fn move_to_ready(
        &self,
        ready_state: Arc<ReadyState>,
        closed_rx: oneshot::Receiver<Result<(), String>>,
    ) {
        let on_drop;
        {
            let mut data = self.data.lock().unwrap();
            // Reset connection backoff upon successfully moving to ready.
            data.backoff.reset();
            on_drop = data.on_drop.clone();
            data.update_state(InternalSubchannelState::Ready(ready_state));
        }
        // TODO(easwars): Does it make sense for disconnected() to return an
        // error string containing information about why the connection
        // terminated? But what can we do with that error other than logging
        // it, which the transport can do as well?
        tokio::select! {
            _ = on_drop.notified() => {}
            e = closed_rx => {
                eprintln!("Transport closed: {e:?}");
                self.move_to_idle();
            }
        }
    }

    // Sets the state to transient failure and then waits until the subchannel
    // is dropped or the backoff expires.  Moves to idle upon backoff expiry.
    async fn move_to_transient_failure(&self, err: String) {
        let runtime;
        let on_drop;
        let backoff_interval;
        {
            let mut data = self.data.lock().unwrap();
            data.update_state(InternalSubchannelState::TransientFailure(err.clone()));
            backoff_interval = data.backoff.backoff_until();
            runtime = data.runtime.clone();
            on_drop = data.on_drop.clone();
        }
        tokio::select! {
            _ = on_drop.notified() => {}
            _ = runtime.sleep(backoff_interval.saturating_duration_since(Instant::now())) => {
                self.move_to_idle();
            }
        }
    }
}

impl Drop for InternalSubchannel {
    fn drop(&mut self) {
        let unregister_fn = self.unregister_fn.take();
        unregister_fn.unwrap()(self.key.clone());
        self.on_drop.notify_waiters();
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

struct FailingSendStream {}

impl SendStream for FailingSendStream {
    async fn send(&mut self, _msg: &dyn SendMessage, _options: SendOptions) -> Result<(), ()> {
        Err(())
    }
}

struct FailingRecvStream {
    status: Option<Status>,
}

impl RecvStream for FailingRecvStream {
    async fn next(&mut self, _msg: &mut dyn RecvMessage) -> ClientResponseStreamItem {
        match self.status.take() {
            Some(status) => ClientResponseStreamItem::Trailers(Trailers::new(status)),
            None => ClientResponseStreamItem::StreamClosed,
        }
    }
}

impl FailingRecvStream {
    fn new(status: Status) -> Self {
        FailingRecvStream {
            status: Some(status),
        }
    }
}

fn create_call_details(authority: &Authority, full_method: &str) -> CallDetails {
    let (service, method) = full_method.rsplit_once('/').unwrap_or((full_method, ""));
    let host_str = authority.host();

    let host = match authority.port() {
        Some(443) | None => host_str.to_string(),
        // Add [] for IPv6 addresses.
        Some(port) if host_str.contains(':') => {
            format!("[{}]:{}", host_str, port)
        }
        Some(port) => format!("{}:{}", host_str, port),
    };

    CallDetails::new(format!("https://{}{}", host, service), method.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_call_details() {
        let authority = Authority::new("localhost", None);
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("localhost", Some(50051));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost:50051/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("localhost", Some(443));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://localhost/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("::1", Some(50051));
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://[::1]:50051/service");
        assert_eq!(details.method_name(), "method");

        let authority = Authority::new("::1", None);
        let details = create_call_details(&authority, "/service/method");
        assert_eq!(details.service_url(), "https://::1/service");
        assert_eq!(details.method_name(), "method");
    }
}
