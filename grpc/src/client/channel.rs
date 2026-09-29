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

//! The client-side gRPC Channel implementation.

use core::panic;
use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::StatusCodeError;
use crate::StatusError;
use crate::client::CallOptions;
use crate::client::ConnectivityState;
use crate::client::DynInvoke;
use crate::client::DynRecvStream;
use crate::client::DynSendStream;
use crate::client::Invoke;
use crate::client::RequestHeaders;
use crate::client::load_balancing::LbPolicy as _;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::QueuingPicker;
use crate::client::load_balancing::WorkData;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::graceful_switch::GracefulSwitchLbConfig;
use crate::client::load_balancing::graceful_switch::GracefulSwitchPolicy;
use crate::client::load_balancing::pick_first;
use crate::client::load_balancing::round_robin;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::load_balancing::subchannel_sharing::SubchannelSharing;
use crate::client::load_balancing::{self};
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::dns;
use crate::client::name_resolution::global_registry;
use crate::client::name_resolution::proxy_resolver;
use crate::client::name_resolution::{self};
use crate::client::service_config::ParseResult;
use crate::client::service_config::ServiceConfig;
use crate::client::stream_util::FailingRecvStream;
use crate::client::subchannel::InternalSubchannel;
use crate::client::subchannel::NopBackoff;
use crate::client::transport::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::SecurityOpts;
use crate::client::transport::TransportRegistry;
#[cfg(feature = "_runtime-tokio")]
use crate::client::transport::tonic as tonic_transport;
use crate::core::Address;
use crate::credentials::ChannelCredentials;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::common::Authority;
use crate::rt;
use crate::rt::GrpcRuntime;
#[cfg(feature = "_runtime-tokio")]
use crate::rt::default_runtime;

/// A virtual, persistent connection to a gRPC service.
///
/// A `Channel` begins in an "idle" state and connects when the first RPC is
/// made or its [`get_state`](Channel::get_state) method is called.
///
/// To perform RPCs, use the [`Invoke`] trait implemented by `Channel`.
#[derive(Clone)]
pub struct Channel {
    inner: Arc<PersistentChannel>,
}

impl Channel {
    /// Creates a new channel builder for the given target. Target and
    /// credentials are required to build a channel.
    ///
    /// The [target name](https://github.com/grpc/grpc/blob/master/doc/naming.md)
    /// is a fully qualified, self contained URI defined by [rfc3986](https://datatracker.ietf.org/doc/html/rfc3986).
    /// The target's scheme determines the name resolver used. If none is
    /// detected the default name resolver ("dns") is used, unless overridden
    /// by the user. Valid examples of target names include:
    ///
    /// "foo.googleapis.com:8080"
    /// "dns:///foo.googleapis.com:8080"
    /// "dns:///foo.googleapis.com"
    /// "dns:///10.0.0.213:8080"
    /// "dns:///%5B2001:db8:85a3:8d3:1319:8a2e:370:7348%5D:443"
    /// "dns://8.8.8.8/foo.googleapis.com:8080"
    /// "dns://8.8.8.8/foo.googleapis.com"
    /// "zookeeper://zk.example.com:9900/example_service"
    ///
    /// Credentials must implement the [`ChannelCredentials`] trait.
    ///
    ///
    ///# Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use grpc::client::Channel;
    /// use grpc::credentials::LocalChannelCredentials;
    ///
    /// let channel = Channel::builder("dns:///localhost:123", Arc::new(LocalChannelCredentials::new()))
    ///     .build();
    /// ```
    #[cfg(feature = "_runtime-tokio")]
    pub fn builder(
        target: impl Into<String>,
        credentials: Arc<dyn ChannelCredentials>,
    ) -> ChannelBuilder {
        ChannelBuilder {
            target: target.into(),
            credentials,
            authority: None,
            runtime: default_runtime(),
        }
    }

    /// Returns the current state of the channel. If `connect` is true and the
    /// state was [`Idle`](ConnectivityState::Idle), the channel will attempt to
    /// create connections.
    pub fn get_state(&mut self, connect: bool) -> ConnectivityState {
        self.inner.get_state(connect)
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
}

impl Invoke for Channel {
    type SendStream = Box<dyn DynSendStream>;
    type RecvStream = Box<dyn DynRecvStream>;

    async fn invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        let ac = self.inner.get_active_channel();
        ac.invoke(headers, options).await
    }
}

pub struct ChannelBuilder {
    // Required values.
    target: String,
    credentials: Arc<dyn ChannelCredentials>,
    runtime: GrpcRuntime,

    // Optional values.
    authority: Option<String>,
}

impl ChannelBuilder {
    /// Builds the channel with the provided configuration.
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use grpc::client::Channel;
    /// use grpc::credentials::LocalChannelCredentials;
    ///
    /// let channel = Channel::builder("dns:///localhost:123", Arc::new(LocalChannelCredentials::new()))
    ///     .build();
    /// ```
    pub fn build(self) -> Channel {
        // TODO(nathanielford) This construction is currently a rough-cut placeholder.
        // The design of PersistentChannel and how and where it is initialized
        // will be finalized with the 'Internal Channel Design' with
        // consideration for:
        // - error handling (inc. always-failing resolvers due to invalid targets))
        // - testing (inc. credential and transport configuration)

        setup_registeries();

        // TODO(nathanielford): Return errors here instead of panicking.
        let target = Target::from_str(self.target.as_str()).unwrap();
        let resolver_builder = global_registry().get(target.scheme()).unwrap();
        let resolver_builder = proxy_resolver::Builder::new_arc(resolver_builder);

        let authority = self
            .authority
            .unwrap_or_else(|| resolver_builder.default_authority(&target).to_owned());
        let security_opts = SecurityOpts {
            credentials: self.credentials,
            authority: Authority::from_host_port_str(&authority),
            handshake_info: ClientHandshakeInfo::default(),
        };
        Channel {
            inner: Arc::new(PersistentChannel {
                active_channel: Mutex::default(),
                target,
                security_opts,
                runtime: self.runtime,
                resolver_builder,
            }),
        }
    }

    /// Sets the channel's authority value, to be used as the :authority
    /// pseudo-header and the server name in authentication handshakes. This
    /// overrides all other ways of setting authority on the channel, but can be
    /// overridden by per-call authority values.
    pub fn authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = Some(authority.into());
        self
    }
}

struct PersistentChannel {
    active_channel: Mutex<Option<Arc<ActiveChannel>>>,

    // Configuration
    target: Target,
    security_opts: SecurityOpts,
    runtime: GrpcRuntime,

    // Inferred Configuration
    resolver_builder: Arc<dyn ResolverBuilder>,
}

impl PersistentChannel {
    /// Returns the current state of the channel. If there is no underlying active
    /// channel, returns Idle. If `connect` is true, will create a new active
    /// channel iff none exists.
    fn get_state(&self, connect: bool) -> ConnectivityState {
        // Done this away to avoid potentially locking twice.
        let active_channel = if connect {
            self.get_active_channel()
        } else {
            match self.active_channel.lock().unwrap().clone() {
                Some(x) => x,
                None => {
                    return ConnectivityState::Idle;
                }
            }
        };

        active_channel.lb_watcher.cur().connectivity_state
    }

    /// Gets the underlying active channel. If there is no current connection,
    /// it will create one. This cannot fail and will always return a valid
    /// active channel.
    fn get_active_channel(&self) -> Arc<ActiveChannel> {
        let mut active_channel = self.active_channel.lock().unwrap();

        if active_channel.is_none() {
            *active_channel = Some(ActiveChannel::new_arc_for(self));
        }

        active_channel.clone().unwrap() // We have ensured this is not None.
    }
}

// A channel that is not idle (connecting, ready, or erroring).
struct ActiveChannel {
    abort_handle: Box<dyn rt::TaskHandle>, /* Work scheduler task killed when ActiveChannel is
                                            * dropped. */
    lb_watcher: Arc<Watcher<LbState>>, /* For getting the channel connectivity state and pickers
                                        * for RPCs. */
}

impl ActiveChannel {
    fn new_arc_for(persistent_channel: &PersistentChannel) -> Arc<Self> {
        let runtime = persistent_channel.runtime.clone();

        let lb_watcher = Arc::new(Watcher::new(LbState {
            connectivity_state: ConnectivityState::Connecting,
            picker: Arc::new(QueuingPicker) as Arc<dyn Picker>,
        }));

        let (wqtx, mut wqrx) = mpsc::unbounded_channel::<WorkQueueItem>();
        let mut resolver_channel_controller = ResolverChannelController::new(
            wqtx.clone(),
            runtime.clone(),
            lb_watcher.clone(),
            persistent_channel.security_opts.clone(),
        );

        let work_scheduler = Arc::new(ResolverWorkScheduler { wqtx });
        let resolver_opts = name_resolution::ResolverOptions {
            authority: persistent_channel
                .security_opts
                .authority
                .host_port_string(),
            work_scheduler,
            runtime: runtime.clone(),
        };
        let mut resolver = persistent_channel
            .resolver_builder
            .build(&persistent_channel.target, resolver_opts);

        let abort_handle = runtime.spawn(Box::pin(async move {
            while let Some(w) = wqrx.recv().await {
                match w {
                    WorkQueueItem::ScheduleResolver => {
                        resolver.work(&mut resolver_channel_controller);
                    }
                    WorkQueueItem::ResolveNow => resolver.resolve_now(),
                    WorkQueueItem::ScheduleLbPolicy(data) => {
                        resolver_channel_controller
                            .lb_policy
                            .work(data, &mut resolver_channel_controller.lb_channel_controller);
                    }
                }
            }
        }));

        Arc::new(Self {
            abort_handle,
            lb_watcher,
        })
    }
}

impl Invoke for Arc<ActiveChannel> {
    type SendStream = Box<dyn DynSendStream>;
    type RecvStream = Box<dyn DynRecvStream>;

    async fn invoke(
        &self,
        headers: RequestHeaders,
        options: CallOptions,
    ) -> (Self::SendStream, Self::RecvStream) {
        let mut i = self.lb_watcher.iter();
        loop {
            let Some(state) = i.next().await else {
                return FailingRecvStream::new_stream_pair(
                    StatusError::new(StatusCodeError::Internal, "channel has been closed"),
                    None,
                );
            };
            let result = &state.picker.pick(&headers);
            match result {
                PickResult::Pick(pr) => {
                    if let Some(sc) = pr.subchannel.downcast_ref::<InternalSubchannel>() {
                        return sc.dyn_invoke(headers, options).await;
                    } else {
                        panic!(
                            "picked subchannel is not an implementation provided by the channel"
                        );
                    }
                }
                PickResult::Queue => {
                    // Continue and retry the RPC with the next picker.
                }
                PickResult::Fail(status) => {
                    return FailingRecvStream::new_stream_pair(status.clone(), None);
                }
                PickResult::Drop(status) => {
                    todo!("dropped pick: {:?}", status);
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

struct ResolverChannelController {
    wqtx: WorkQueueTx, // To queue re-resolution requests
    runtime: GrpcRuntime,
    lb_policy: SubchannelSharing<GracefulSwitchPolicy>,
    lb_work_scheduler: Arc<LbWorkScheduler>,
    lb_channel_controller: LbChannelController,
}

impl ResolverChannelController {
    fn new(
        wqtx: WorkQueueTx,
        runtime: GrpcRuntime,
        lb_watcher: Arc<Watcher<LbState>>,
        security_opts: SecurityOpts,
    ) -> Self {
        let lb_work_scheduler = Arc::new(LbWorkScheduler { wqtx: wqtx.clone() });
        let lb_channel_controller = LbChannelController {
            lb_work_scheduler: lb_work_scheduler.clone(),
            transport_registry: GLOBAL_TRANSPORT_REGISTRY.clone(),
            lb_watcher,
            runtime: runtime.clone(),
            security_opts,
        };
        Self {
            lb_policy: SubchannelSharing::new(
                GracefulSwitchPolicy::new(runtime.clone(), lb_work_scheduler.clone()),
                lb_work_scheduler.clone(),
            ),
            lb_work_scheduler,
            lb_channel_controller,
            wqtx: wqtx.clone(),
            runtime: runtime.clone(),
        }
    }
}

impl name_resolution::ChannelController for ResolverChannelController {
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
        let (builder, config) = match update.service_config.as_ref() {
            Ok(Some(sc)) => sc.lb_config(),
            _ => ServiceConfig::default_lb_policy(),
        };

        let gsb_config = GracefulSwitchLbConfig::new(builder, config);

        self.lb_policy
            .resolver_update(update, &gsb_config, &mut self.lb_channel_controller)
    }

    fn parse_service_config(&self, config: &str) -> ParseResult {
        ServiceConfig::parse(config)
    }
}

struct LbChannelController {
    lb_work_scheduler: Arc<LbWorkScheduler>,
    transport_registry: TransportRegistry, // For creating subchannels
    lb_watcher: Arc<Watcher<LbState>>,
    runtime: GrpcRuntime, // For creating subchannels
    security_opts: SecurityOpts,
}

impl load_balancing::ChannelController for LbChannelController {
    fn new_subchannel(
        &mut self,
        address: &Address,
        work_scheduler: Arc<dyn WorkScheduler>,
    ) -> (Arc<dyn Subchannel>, SubchannelState) {
        let transport = self
            .transport_registry
            .get_transport(address.network_type)
            .unwrap();
        (
            InternalSubchannel::new_arc(
                address.clone(),
                transport,
                Arc::new(NopBackoff {}),
                self.runtime.clone(),
                self.security_opts.clone(),
                work_scheduler,
            ),
            SubchannelState::idle(),
        )
    }

    fn update_picker(&mut self, update: LbState) {
        self.lb_watcher.update(update);
    }

    fn request_resolution(&mut self) {
        _ = self.lb_work_scheduler.wqtx.send(WorkQueueItem::ResolveNow);
    }
}

#[derive(Debug)]
struct LbWorkScheduler {
    wqtx: WorkQueueTx,
}

impl WorkScheduler for LbWorkScheduler {
    fn schedule_work(&self, data: Option<WorkData>) {
        _ = self.wqtx.send(WorkQueueItem::ScheduleLbPolicy(data));
    }
}

pub(super) enum WorkQueueItem {
    // Call the LB policy to do work.  This includes subchannel state updates,
    // which are delivered as WorkData by the subchannels themselves.
    ScheduleLbPolicy(Option<WorkData>),
    // Call the resolver to do work.
    ScheduleResolver,
    // Call the resolver to resolve now.
    ResolveNow,
}

pub(crate) struct Todo;

// Enables multiple receivers to view data output from a single producer.
// Producer calls update.  Consumers call iter() and call next() until they find
// a good value or encounter None.
pub(crate) struct Watcher<T> {
    tx: watch::Sender<T>,
    rx: watch::Receiver<T>,
}

impl<T: Clone> Watcher<T> {
    fn new(initial: T) -> Self {
        let (tx, rx) = watch::channel(initial);
        Self { tx, rx }
    }

    pub(crate) fn iter(&self) -> WatcherIter<T> {
        let mut rx = self.rx.clone();
        rx.mark_changed();
        WatcherIter { rx }
    }

    pub(crate) fn cur(&self) -> T {
        let mut rx = self.rx.clone();
        rx.mark_changed();
        rx.borrow().clone()
    }

    fn update(&self, item: T) {
        self.tx.send(item).unwrap();
    }
}

pub(crate) struct WatcherIter<T> {
    rx: watch::Receiver<T>,
}
// TODO: Use an arc_swap::ArcSwap instead that contains T and a channel closed
// when T is updated.  Even if the channel needs a lock, the fast path becomes
// lock-free.

impl<T: Clone> WatcherIter<T> {
    /// Returns the next unseen value
    pub(crate) async fn next(&mut self) -> Option<T> {
        self.rx.changed().await.ok()?;
        Some(self.rx.borrow_and_update().clone())
    }
}

/// Parses the host and port from a string. When the input can not be parsed
/// as (host, port) pair, it returns the entire input as the host.
fn parse_authority(host_and_port: &str) -> Authority {
    // Handle bracketed IPv6 addresses (e.g., "[::1]:80").
    if let Some(stripped) = host_and_port.strip_prefix('[')
        && let Some((host, port_str)) = stripped.split_once("]:")
        && let Ok(port) = port_str.parse::<u16>()
    {
        return Authority::new(host, Some(port));
    }
    // Handle unbracketed addresses (IPv4 or hostnames, e.g., "localhost:8080").
    if let Some((host, port_str)) = host_and_port.rsplit_once(':')
        && !host.contains(':')
        && let Ok(port) = port_str.parse::<u16>()
    {
        return Authority::new(host, Some(port));
    }
    Authority::new(host_and_port.to_string(), None)
}

// Sets up the default registries for transports, name resolvers, and load
// balancers.
fn setup_registeries() {
    pick_first::reg();
    round_robin::reg();
    dns::reg();
    #[cfg(unix)]
    name_resolution::unix::reg();
    #[cfg(target_os = "linux")]
    name_resolution::unix_abstract::reg();
    #[cfg(feature = "_runtime-tokio")]
    tonic_transport::reg();
}
