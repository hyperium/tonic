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
use std::{
    any::Any,
    error::Error,
    mem,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
    vec,
};

use hyper::client::conn;
use tokio::sync::{mpsc, watch, Notify};

use serde_json::json;
use url::Url; // NOTE: http::Uri requires non-empty authority portion of URI

use crate::attributes::Attributes;
use crate::rt;
use crate::service::{Request, Response, Service};
use crate::{client::ConnectivityState, rt::Runtime};
use crate::{credentials::Credentials, rt::default_runtime};

use super::name_resolution::{self, global_registry, Address, ResolverUpdate};
use super::service_config::ServiceConfig;
use super::transport::{TransportRegistry, GLOBAL_TRANSPORT_REGISTRY};
use super::{
    load_balancing::{
        self, pick_first, ExternalSubchannel, LbPolicy, LbPolicyBuilder, LbPolicyOptions, LbState,
        ParsedJsonLbConfig, PickResult, Picker, Subchannel, SubchannelState, WorkScheduler,
        GLOBAL_LB_REGISTRY,
    },
    subchannel::{
        InternalSubchannel, InternalSubchannelPool, NopBackoff, SubchannelKey,
        SubchannelStateWatcher,
    },
};

#[non_exhaustive]
pub struct ChannelOptions {
    pub transport_options: Attributes, // ?
    pub override_authority: Option<String>,
    pub connection_backoff: Option<TODO>,
    pub default_service_config: Option<String>,
    pub disable_proxy: bool,
    pub disable_service_config_lookup: bool,
    pub disable_health_checks: bool,
    pub max_retry_memory: u32, // ?
    pub idle_timeout: Duration,
    // TODO: pub transport_registry: Option<TransportRegistry>,
    // TODO: pub name_resolver_registry: Option<ResolverRegistry>,
    // TODO: pub lb_policy_registry: Option<LbPolicyRegistry>,

    // Typically we allow settings at the channel level that impact all RPCs,
    // but can also be set per-RPC.  E.g.s:
    //
    // - interceptors
    // - user-agent string override
    // - max message sizes
    // - max retry/hedged attempts
    // - disable retry
    //
    // In gRPC-Go, we can express CallOptions as DialOptions, which is a nice
    // pattern: https://pkg.go.dev/google.golang.org/grpc#WithDefaultCallOptions
    //
    // To do this in rust, all optional behavior for a request would need to be
    // expressed through a trait that applies a mutation to a request.  We'd
    // apply all those mutations before the user's options so the user's options
    // would override the defaults, or so the defaults would occur first.
    pub default_request_extensions: Vec<Box<TODO>>, // ??
}

impl Default for ChannelOptions {
    fn default() -> Self {
        Self {
            transport_options: Attributes {},
            override_authority: None,
            connection_backoff: None,
            default_service_config: None,
            disable_proxy: false,
            disable_service_config_lookup: false,
            disable_health_checks: false,
            max_retry_memory: 8 * 1024 * 1024, // 8MB -- ???
            idle_timeout: Duration::from_secs(30 * 60),
            default_request_extensions: vec![],
        }
    }
}

impl ChannelOptions {
    pub fn transport_options(self, transport_options: TODO) -> Self {
        todo!(); // add to existing options.
    }
    pub fn override_authority(self, authority: String) -> Self {
        Self {
            override_authority: Some(authority),
            ..self
        }
    }
    // etc
}

// All of Channel needs to be thread-safe.  Arc<inner>?  Or give out
// Arc<Channel> from constructor?
#[derive(Clone)]
pub struct Channel {
    inner: Arc<PersistentChannel>,
}

impl Channel {
    /// Constructs a new gRPC channel.  A gRPC channel is a virtual, persistent
    /// connection to a service.  Channel creation cannot fail, but if the
    /// target string is invalid, the returned channel will never connect, and
    /// will fail all RPCs.
    // TODO: should this return a Result instead?
    pub fn new(
        target: &str,
        credentials: Option<Box<dyn Credentials>>,
        options: ChannelOptions,
    ) -> Self {
        pick_first::reg();
        Self {
            inner: Arc::new(PersistentChannel::new(
                target,
                credentials,
                default_runtime(),
                options,
            )),
        }
    }

    // TODO: enter_idle(&self) and graceful_stop()?

    /// Returns the current state of the channel. Any errors translate into a
    /// TransientFailure state.
    pub fn state(&mut self, connect: bool) -> ConnectivityState {
        let state = self.inner.state(connect);
        match state {
            Ok(s) => s,
            Err(_) => ConnectivityState::TransientFailure,
        }
    }

    /// Waits for the state of the channel to change from source.  Times out and
    /// returns an error after the deadline.
    pub async fn wait_for_state_change(
        &self,
        source: ConnectivityState,
        deadline: Instant,
    ) -> Result<(), Box<dyn Error>> {
        todo!()
    }

    pub async fn call(&self, method: String, request: Request) -> Response {
        let ac = self.inner.get_active_channel(true).unwrap();
        ac.call(method, request).await
    }
}

// A PersistentChannel represents the static configuration of a channel and an
// optional Arc of an ActiveChannel.  An ActiveChannel exists whenever the
// PersistentChannel is not IDLE.  Every channel is IDLE at creation, or after
// some configurable timeout elapses without any any RPC activity.
struct PersistentChannel {
    target: Url,
    options: ChannelOptions,
    active_channel: Mutex<Option<Arc<ActiveChannel>>>,
    runtime: Arc<dyn Runtime>,
}

impl PersistentChannel {
    // Channels begin idle so new does not automatically connect.
    // ChannelOptions are only non-required parameters.
    fn new(
        target: &str,
        _credentials: Option<Box<dyn Credentials>>,
        runtime: Arc<dyn rt::Runtime>,
        options: ChannelOptions,
    ) -> Self {
        Self {
            target: Url::from_str(target).unwrap(), // TODO handle err
            active_channel: Mutex::default(),
            options,
            runtime,
        }
    }

    /// Returns the current state of the channel. If there is no underlying active channel,
    /// returns Idle. If `connect` is true, will create a new active channel iff none exists.
    fn state(
        &self,
        connect: bool,
    ) -> Result<ConnectivityState, Box<dyn std::error::Error + Sync + Send>> {
        let ac = self.get_active_channel(connect)?;
        if let Some(s) = ac.connectivity_state.cur() {
            return Ok(s);
        }
        return Ok(ConnectivityState::Idle);
    }

    /// Gets the underlying active channel. If `connect` is true, will create a new channel iff
    /// there is no active channel.
    fn get_active_channel(
        &self,
        connect: bool,
    ) -> Result<Arc<ActiveChannel>, Box<dyn std::error::Error + Sync + Send>> {
        let mut s = self
            .active_channel
            .lock()
            .map_err(|_| "Could not get channel lock.".to_string())?;

        if s.is_none() {
            if connect {
                *s = Some(ActiveChannel::new(
                    self.target.clone(),
                    &self.options,
                    self.runtime.clone(),
                ));
            } else {
                return Err("No active channel.".into());
            }
        }

        s.as_ref()
            .cloned()
            .ok_or_else(|| "Could not clone channel".into())
    }
}

struct ActiveChannel {
    cur_state: Mutex<ConnectivityState>,
    abort_handle: Box<dyn rt::TaskHandle>,
    picker: Arc<Watcher<Arc<dyn Picker>>>,
    connectivity_state: Arc<Watcher<ConnectivityState>>,
    runtime: Arc<dyn Runtime>,
}

impl ActiveChannel {
    fn new(target: Url, options: &ChannelOptions, runtime: Arc<dyn Runtime>) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<WorkQueueItem>();
        let transport_registry = GLOBAL_TRANSPORT_REGISTRY.clone();

        let resolve_now = Arc::new(Notify::new());
        let connectivity_state = Arc::new(Watcher::new());
        let picker = Arc::new(Watcher::new());
        let mut channel_controller = InternalChannelController::new(
            transport_registry,
            resolve_now.clone(),
            tx.clone(),
            picker.clone(),
            connectivity_state.clone(),
            runtime.clone(),
        );

        let resolver_helper = Box::new(tx.clone());

        // TODO(arjan-bal): Return error here instead of panicking.
        let rb = global_registry().get(target.scheme()).unwrap();
        let target = name_resolution::Target::from(target);
        let authority = target.authority_host_port();
        let authority = if authority.is_empty() {
            rb.default_authority(&target).to_owned()
        } else {
            authority
        };
        let work_scheduler = Arc::new(ResolverWorkScheduler { wqtx: tx });
        let resolver_opts = name_resolution::ResolverOptions {
            authority,
            work_scheduler,
            runtime: runtime.clone(),
        };
        let resolver = rb.build(&target, resolver_opts);

        let jh = runtime.spawn(Box::pin(async move {
            let mut resolver = resolver;
            while let Some(w) = rx.recv().await {
                match w {
                    WorkQueueItem::Closure(func) => func(&mut channel_controller),
                    WorkQueueItem::ScheduleResolver => resolver.work(&mut channel_controller),
                }
            }
        }));

        Arc::new(Self {
            cur_state: Mutex::new(ConnectivityState::Connecting),
            abort_handle: jh,
            picker: picker.clone(),
            connectivity_state: connectivity_state.clone(),
            runtime,
        })
    }

    async fn call(&self, method: String, request: Request) -> Response {
        // TODO: pre-pick tasks (e.g. deadlines, interceptors, retry)
        let mut i = self.picker.iter();
        loop {
            if let Some(p) = i.next().await {
                let result = &p.pick(&request);
                // TODO: handle picker errors (queue or fail RPC)
                match result {
                    PickResult::Pick(pr) => {
                        if let Some(sc) = (pr.subchannel.as_ref() as &dyn Any)
                            .downcast_ref::<ExternalSubchannel>()
                        {
                            return sc.isc.as_ref().unwrap().call(method, request).await;
                        } else {
                            panic!("picked subchannel is not an implementation provided by the channel");
                        }
                    }
                    PickResult::Queue => {
                        // Continue and retry the RPC with the next picker.
                    }
                    PickResult::Fail(status) => {
                        panic!("failed pick: {}", status);
                    }
                    PickResult::Drop(status) => {
                        panic!("dropped pick: {}", status);
                    }
                }
            }
        }
    }
}

impl Drop for ActiveChannel {
    fn drop(&mut self) {
        self.abort_handle.abort();
    }
}

struct ResolverWorkScheduler {
    wqtx: WorkQueueTx,
}

pub(super) type WorkQueueTx = mpsc::UnboundedSender<WorkQueueItem>;

impl name_resolution::WorkScheduler for ResolverWorkScheduler {
    fn schedule_work(&self) {
        let _ = self.wqtx.send(WorkQueueItem::ScheduleResolver);
    }
}

pub(crate) struct InternalChannelController {
    pub(super) lb: Arc<GracefulSwitchBalancer>, // called and passes mutable parent to it, so must be Arc.
    transport_registry: TransportRegistry,
    pub(super) subchannel_pool: Arc<InternalSubchannelPool>,
    resolve_now: Arc<Notify>,
    wqtx: WorkQueueTx,
    picker: Arc<Watcher<Arc<dyn Picker>>>,
    connectivity_state: Arc<Watcher<ConnectivityState>>,
    runtime: Arc<dyn Runtime>,
}

impl InternalChannelController {
    fn new(
        transport_registry: TransportRegistry,
        resolve_now: Arc<Notify>,
        wqtx: WorkQueueTx,
        picker: Arc<Watcher<Arc<dyn Picker>>>,
        connectivity_state: Arc<Watcher<ConnectivityState>>,
        runtime: Arc<dyn Runtime>,
    ) -> Self {
        let lb = Arc::new(GracefulSwitchBalancer::new(wqtx.clone(), runtime.clone()));

        Self {
            lb,
            transport_registry,
            subchannel_pool: Arc::new(InternalSubchannelPool::new()),
            resolve_now,
            wqtx,
            picker,
            connectivity_state,
            runtime,
        }
    }

    fn new_esc_for_isc(&self, isc: Arc<InternalSubchannel>) -> Arc<dyn Subchannel> {
        let sc = Arc::new(ExternalSubchannel::new(isc.clone(), self.wqtx.clone()));
        let watcher = Arc::new(SubchannelStateWatcher::new(sc.clone(), self.wqtx.clone()));
        sc.set_watcher(watcher.clone());
        isc.register_connectivity_state_watcher(watcher.clone());
        sc
    }
}

impl name_resolution::ChannelController for InternalChannelController {
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
        let lb = self.lb.clone();
        lb.handle_resolver_update(update, self)
            .map_err(|err| err.to_string())
    }

    fn parse_service_config(&self, config: &str) -> Result<ServiceConfig, String> {
        Err("service configs not supported".to_string())
    }
}

impl load_balancing::ChannelController for InternalChannelController {
    fn new_subchannel(&mut self, address: &Address) -> Arc<dyn Subchannel> {
        let key = SubchannelKey::new(address.clone());
        if let Some(isc) = self.subchannel_pool.lookup_subchannel(&key) {
            return self.new_esc_for_isc(isc);
        }

        // If we get here, it means one of two things:
        // 1. provided key is not found in the map
        // 2. provided key points to an unpromotable value, which can occur if
        //    its internal subchannel has been dropped but hasn't been
        //    unregistered yet.

        let transport = self
            .transport_registry
            .get_transport(address.network_type)
            .unwrap();
        let scp = self.subchannel_pool.clone();
        let isc = InternalSubchannel::new(
            key.clone(),
            transport,
            Arc::new(NopBackoff {}),
            Box::new(move |k: SubchannelKey| {
                scp.unregister_subchannel(&k);
            }),
            self.runtime.clone(),
        );
        let _ = self.subchannel_pool.register_subchannel(&key, isc.clone());
        self.new_esc_for_isc(isc)
    }

    fn update_picker(&mut self, update: LbState) {
        println!(
            "update picker called with state: {:?}",
            update.connectivity_state
        );
        self.picker.update(update.picker);
        self.connectivity_state.update(update.connectivity_state);
    }

    fn request_resolution(&mut self) {
        self.resolve_now.notify_one();
    }
}

// A channel that is not idle (connecting, ready, or erroring).
#[derive(Debug)]
pub(super) struct GracefulSwitchBalancer {
    pub(super) policy: Mutex<Option<Box<dyn LbPolicy>>>,
    policy_builder: Mutex<Option<Arc<dyn LbPolicyBuilder>>>,
    work_scheduler: WorkQueueTx,
    pending: Mutex<bool>,
    runtime: Arc<dyn Runtime>,
}

impl WorkScheduler for GracefulSwitchBalancer {
    fn schedule_work(&self) {
        if mem::replace(&mut *self.pending.lock().unwrap(), true) {
            // Already had a pending call scheduled.
            return;
        }
        let _ = self.work_scheduler.send(WorkQueueItem::Closure(Box::new(
            |c: &mut InternalChannelController| {
                *c.lb.pending.lock().unwrap() = false;
                c.lb.clone()
                    .policy
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .work(c);
            },
        )));
    }
}

impl GracefulSwitchBalancer {
    fn new(work_scheduler: WorkQueueTx, runtime: Arc<dyn Runtime>) -> Self {
        Self {
            policy_builder: Mutex::default(),
            policy: Mutex::default(), // new(None::<Box<dyn LbPolicy>>),
            work_scheduler,
            pending: Mutex::default(),
            runtime,
        }
    }

    fn handle_resolver_update(
        self: &Arc<Self>,
        update: ResolverUpdate,
        controller: &mut InternalChannelController,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if update.service_config.as_ref().is_ok_and(|sc| sc.is_some()) {
            return Err("can't do service configs yet".into());
        }
        let policy_name = pick_first::POLICY_NAME;
        let mut p = self.policy.lock().unwrap();
        if p.is_none() {
            let builder = GLOBAL_LB_REGISTRY.get_policy(policy_name).unwrap();
            let newpol = builder.build(LbPolicyOptions {
                work_scheduler: self.clone(),
                runtime: self.runtime.clone(),
            });
            *self.policy_builder.lock().unwrap() = Some(builder);
            *p = Some(newpol);
        }

        // TODO: config should come from ServiceConfig.
        let builder = self.policy_builder.lock().unwrap();
        let config = match builder
            .as_ref()
            .unwrap()
            .parse_config(&ParsedJsonLbConfig::from_value(
                json!({"shuffleAddressList": true, "unknown_field": false}),
            )) {
            Ok(cfg) => cfg,
            Err(e) => {
                return Err(e);
            }
        };

        p.as_mut()
            .unwrap()
            .resolver_update(update, config.as_ref(), controller)

        // TODO: close old LB policy gracefully vs. drop?
    }
    pub(super) fn subchannel_update(
        &self,
        subchannel: Arc<dyn Subchannel>,
        state: &SubchannelState,
        channel_controller: &mut dyn load_balancing::ChannelController,
    ) {
        let mut p = self.policy.lock().unwrap();

        p.as_mut()
            .unwrap()
            .subchannel_update(subchannel, state, channel_controller);
    }
}

pub(super) enum WorkQueueItem {
    // Execute the closure.
    Closure(Box<dyn FnOnce(&mut InternalChannelController) + Send + Sync>),
    // Call the resolver to do work.
    ScheduleResolver,
}

pub struct TODO;

// Enables multiple receivers to view data output from a single producer.
// Producer calls update.  Consumers call iter() and call next() until they find
// a good value or encounter None.
pub(crate) struct Watcher<T> {
    tx: watch::Sender<Option<T>>,
    rx: watch::Receiver<Option<T>>,
}

impl<T: Clone> Watcher<T> {
    fn new() -> Self {
        let (tx, rx) = watch::channel(None);
        Self { tx, rx }
    }

    pub(crate) fn iter(&self) -> WatcherIter<T> {
        let mut rx = self.rx.clone();
        rx.mark_changed();
        WatcherIter { rx }
    }

    pub(crate) fn cur(&self) -> Option<T> {
        let mut rx = self.rx.clone();
        rx.mark_changed();
        let c = rx.borrow();
        c.clone()
    }

    fn update(&self, item: T) {
        self.tx.send(Some(item)).unwrap();
    }
}

pub(crate) struct WatcherIter<T> {
    rx: watch::Receiver<Option<T>>,
}
// TODO: Use an arc_swap::ArcSwap instead that contains T and a channel closed
// when T is updated.  Even if the channel needs a lock, the fast path becomes
// lock-free.

impl<T: Clone> WatcherIter<T> {
    /// Returns the next unseen value
    pub(crate) async fn next(&mut self) -> Option<T> {
        loop {
            self.rx.changed().await.ok()?;
            let x = self.rx.borrow_and_update();
            if x.is_some() {
                return x.clone();
            }
        }
    }
}
