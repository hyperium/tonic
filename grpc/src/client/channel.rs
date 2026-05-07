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
use std::error::Error;
use std::mem;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::watch;
use url::Url; // NOTE: http::Uri requires non-empty authority portion of URI

use crate::StatusCodeError;
use crate::StatusError;
use crate::client::CallOptions;
use crate::client::ConnectivityState;
use crate::client::DynInvoke;
use crate::client::DynRecvStream;
use crate::client::DynSendStream;
use crate::client::Invoke;
use crate::client::load_balancing::LbPolicy as _;
use crate::client::load_balancing::LbState;
use crate::client::load_balancing::ParsedJsonLbConfig;
use crate::client::load_balancing::PickResult;
use crate::client::load_balancing::Picker;
use crate::client::load_balancing::QueuingPicker;
use crate::client::load_balancing::WorkScheduler;
use crate::client::load_balancing::graceful_switch::GracefulSwitchPolicy;
use crate::client::load_balancing::pick_first;
use crate::client::load_balancing::round_robin;
use crate::client::load_balancing::subchannel::Subchannel;
use crate::client::load_balancing::subchannel::SubchannelState;
use crate::client::load_balancing::subchannel_sharing::SubchannelSharing;
use crate::client::load_balancing::{self};
use crate::client::name_resolution::Address;
use crate::client::name_resolution::ResolverBuilder;
use crate::client::name_resolution::ResolverUpdate;
use crate::client::name_resolution::Target;
use crate::client::name_resolution::dns;
use crate::client::name_resolution::global_registry;
use crate::client::name_resolution::{self};
use crate::client::service_config::LbPolicyType;
use crate::client::service_config::ServiceConfig;
use crate::client::stream_util::FailingRecvStream;
use crate::client::subchannel::InternalSubchannel;
use crate::client::subchannel::NopBackoff;
use crate::client::transport::GLOBAL_TRANSPORT_REGISTRY;
use crate::client::transport::SecurityOpts;
use crate::client::transport::TransportRegistry;
#[cfg(feature = "_runtime-tokio")]
use crate::client::transport::tonic as tonic_transport;
use crate::core::RequestHeaders;
use crate::credentials::ChannelCredentials;
use crate::credentials::client::ClientHandshakeInfo;
use crate::credentials::common::Authority;
use crate::rt;
use crate::rt::GrpcEndpoint;
use crate::rt::GrpcRuntime;
use crate::rt::default_runtime;

pub struct MissingOption;
pub struct PresentOption<T>(pub T);

// All of Channel needs to be thread-safe.  Arc<inner>?  Or give out
// Arc<Channel> from constructor?
#[derive(Clone)]
pub struct Channel {
    inner: Arc<PersistentChannel>,
}

impl Channel {
    pub fn builder<CrState, RtState>(
        target: String,
    ) -> ChannelBuilder<MissingOption, MissingOption> {
        ChannelBuilder {
            target: target.into(),
            credentials: MissingOption,
            runtime: MissingOption,
            default_service_config: None,
        }
    }
    // TODO: enter_idle(&self) and graceful_stop()?

    /// Returns the current state of the channel. If there is no underlying active channel,
    /// returns Idle. If `connect` is true, will create a new active channel.
    pub fn state(&mut self, connect: bool) -> ConnectivityState {
        self.inner.state(connect)
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

pub struct ChannelBuilder<CrState, RtState> {
    // Required builder parameters:
    target: String,
    credentials: CrState,
    runtime: RtState, // Optional if the Tokio flag is present to default to.
    // Optional builder parameters with defaults:
    default_service_config: Option<String>,
}

impl<CrState, RtState> ChannelBuilder<CrState, RtState> {
    pub fn with_credentials<C>(
        self,
        credentials: impl Into<Arc<C>>,
    ) -> ChannelBuilder<PresentOption<Arc<C>>, RtState> {
        ChannelBuilder {
            target: self.target,
            credentials: PresentOption(credentials.into()),
            runtime: self.runtime,
            default_service_config: self.default_service_config,
        }
    }

    pub fn with_runtime<R>(self, runtime: R) -> ChannelBuilder<CrState, PresentOption<R>> {
        ChannelBuilder {
            target: self.target,
            credentials: self.credentials,
            runtime: PresentOption(runtime),
            default_service_config: self.default_service_config,
        }
    }
}

/// If the Tokio runtime feature is enabled, the channel builder can be built
/// without explicitly providing a runtime, defaulting to the Tokio runtime.
/// This does not prevent a user from providing their own runtime if they wish,
/// and the builder will work as normal.
#[cfg(feature = "_runtime-tokio")]
impl<C> ChannelBuilder<PresentOption<Arc<C>>, MissingOption>
where
    C: ChannelCredentials + 'static,
    C::Output<Box<dyn GrpcEndpoint>>: GrpcEndpoint + 'static,
{
    pub fn build(self) -> Channel {
        self.with_runtime(default_runtime()).build()
    }
}

impl<C> ChannelBuilder<PresentOption<Arc<C>>, PresentOption<GrpcRuntime>>
where
    C: ChannelCredentials + 'static,
    C::Output<Box<dyn GrpcEndpoint>>: GrpcEndpoint + 'static,
{
    // Returns a Channel with the provided parameters.
    // This will not fail, but if the target is invalid, the returned channel
    // will never connect and all RPCs will fail. Further, even if properly
    // configured the channel will begin in IDLE.
    pub fn build(self) -> Channel {
        init_registers();
        let persistent_channel = PersistentChannel::new(
            &self.target,
            self.runtime.0,
            self.credentials.0,
            None, // channel authority
        );

        Channel {
            inner: Arc::new(persistent_channel),
        }
    }
}

// A PersistentChannel represents the static configuration of a channel and an
// optional Arc of an ActiveChannel.  An ActiveChannel exists whenever the
// PersistentChannel is not IDLE.  Every channel is IDLE at creation, or after
// some configurable timeout elapses without any any RPC activity.
struct PersistentChannel {
    active_channel: Mutex<Option<Arc<ActiveChannel>>>,
    target: Target,
    security_opts: SecurityOpts,
    runtime: GrpcRuntime,
    resolver_builder: Arc<dyn ResolverBuilder>,
    authority: String,
}

impl PersistentChannel {
    fn new<C>(
        target: &str,
        runtime: GrpcRuntime,
        credentials: Arc<C>,
        channel_authority: Option<String>,
    ) -> Self
    where
        C: ChannelCredentials,
        C::Output<Box<dyn GrpcEndpoint>>: GrpcEndpoint + 'static,
    {
        // TODO(nford) The entire structure of PersistentChannel and the initialization will be revisited with 'channel internals' design.
        let target = Url::from_str(target).unwrap(); // TODO(nford): Return always-failing channel instead of panicking.
        let resolver_builder = global_registry().get(target.scheme()).unwrap();
        let target = name_resolution::Target::from(target);
        let authority = channel_authority
            .unwrap_or_else(|| resolver_builder.default_authority(&target).to_owned());
        let security_opts = SecurityOpts {
            credentials,
            authority: parse_authority(&authority),
            handshake_info: ClientHandshakeInfo::default(),
        };

        Self {
            target: name_resolution::Target::from(target),
            resolver_builder: resolver_builder,
            active_channel: Mutex::default(),
            runtime: runtime,
            security_opts: security_opts,
            authority: authority,
        }
    }

    /// Returns the current state of the channel. If there is no underlying active channel,
    /// returns Idle. If `connect` is true, will create a new active channel iff none exists.
    fn state(&self, connect: bool) -> ConnectivityState {
        // Done this way to avoid potentially locking twice.
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

    /// Gets the underlying active channel. If there is no current connection, it will create one.
    /// This cannot fail and will always return a valid active channel.
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
    abort_handle: Box<dyn rt::TaskHandle>, // Work scheduler task killed when ActiveChannel is dropped.
    lb_watcher: Arc<Watcher<LbState>>, // For getting the channel connectivity state and pickers for RPCs.
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
            authority: persistent_channel.authority.clone(),
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
                        resolver.work(&mut resolver_channel_controller)
                    }
                    WorkQueueItem::ResolveNow => resolver.resolve_now(),
                    WorkQueueItem::ScheduleLbPolicy => {
                        *resolver_channel_controller
                            .lb_work_scheduler
                            .pending
                            .lock()
                            .unwrap() = false;
                        resolver_channel_controller
                            .lb_policy
                            .work(&mut resolver_channel_controller.lb_channel_controller);
                    }
                    WorkQueueItem::SubchannelStateUpdate { subchannel, state } => {
                        resolver_channel_controller.lb_policy.subchannel_update(
                            subchannel,
                            &state,
                            &mut resolver_channel_controller.lb_channel_controller,
                        );
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
                return FailingRecvStream::new_stream_pair(StatusError::new(
                    StatusCodeError::Internal,
                    "channel has been closed",
                ));
            };
            let result = &state.picker.pick(&headers);
            match result {
                PickResult::Pick(pr) => {
                    if let Some(sc) = pr.subchannel.downcast_ref::<InternalSubchannel>() {
                        return sc.dyn_invoke(headers, options.clone()).await;
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
                    return FailingRecvStream::new_stream_pair(status.clone());
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
        let lb_work_scheduler = Arc::new(LbWorkScheduler {
            pending: Mutex::default(),
            wqtx: wqtx.clone(),
        });
        let lb_channel_controller = LbChannelController {
            lb_work_scheduler: lb_work_scheduler.clone(),
            transport_registry: GLOBAL_TRANSPORT_REGISTRY.clone(),
            wqtx: wqtx.clone(),
            lb_watcher,
            runtime: runtime.clone(),
            security_opts,
        };
        Self {
            lb_policy: SubchannelSharing::new(GracefulSwitchPolicy::new(
                runtime.clone(),
                lb_work_scheduler.clone(),
            )),
            lb_work_scheduler,
            lb_channel_controller,
            wqtx: wqtx.clone(),
            runtime: runtime.clone(),
        }
    }
}

impl name_resolution::ChannelController for ResolverChannelController {
    fn update(&mut self, update: ResolverUpdate) -> Result<(), String> {
        let json_config = if let Ok(Some(service_config)) = update.service_config.as_ref()
            && service_config
                .load_balancing_policy
                .as_ref()
                .is_some_and(|p| *p == LbPolicyType::RoundRobin)
        {
            json!([{round_robin::POLICY_NAME: {}}])
        } else {
            json!([{pick_first::POLICY_NAME: {"shuffleAddressList": true, "unknown_field": false}}])
        };

        // TODO: config should come from ServiceConfig.
        let config =
            GracefulSwitchPolicy::parse_config(&ParsedJsonLbConfig::from_value(json_config))?;

        self.lb_policy
            .resolver_update(update, Some(&config), &mut self.lb_channel_controller)
            .map_err(|err| err.to_string())
    }

    fn parse_service_config(&self, config: &str) -> Result<ServiceConfig, String> {
        Err("service configs not supported".to_string())
    }
}

struct LbChannelController {
    lb_work_scheduler: Arc<LbWorkScheduler>, // Holds `pending` bool (??)
    transport_registry: TransportRegistry,   // For creating subchannels
    wqtx: WorkQueueTx,                       // To queue subchannel state updates
    lb_watcher: Arc<Watcher<LbState>>,
    runtime: GrpcRuntime, // For creating subchanenls
    security_opts: SecurityOpts,
}

impl load_balancing::ChannelController for LbChannelController {
    fn new_subchannel(&mut self, address: &Address) -> (Arc<dyn Subchannel>, SubchannelState) {
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
                self.wqtx.clone(),
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
    pending: Mutex<bool>,
    wqtx: WorkQueueTx,
}

impl WorkScheduler for LbWorkScheduler {
    fn schedule_work(&self) {
        if mem::replace(&mut *self.pending.lock().unwrap(), true) {
            // Already had a pending call scheduled.
            return;
        }
        _ = self.wqtx.send(WorkQueueItem::ScheduleLbPolicy);
    }
}

pub(super) enum WorkQueueItem {
    // Call the LB policy to do work.
    ScheduleLbPolicy,
    // Provide the subchannel state update to the LB policy.
    SubchannelStateUpdate {
        subchannel: Arc<dyn Subchannel>,
        state: SubchannelState,
    },
    // Call the resolver to do work.
    ScheduleResolver,
    // Call the resolver to resolve now.
    ResolveNow,
}

pub struct TODO;

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

/// Parses the host and port from a URL-encoded string. When the input can not
/// be parsed as (host, port) pair, it returns the entire input as the host.
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

// This registers the default implementations of load balancers, name resolvers,
// and transports.
fn init_registers() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_authority() {
        struct TestCase {
            input: &'static str,
            expected: Authority,
        }

        let cases = [
            TestCase {
                input: "localhost:http",
                expected: Authority::new("localhost:http", None),
            },
            TestCase {
                input: "localhost:80",
                expected: Authority::new("localhost", Some(80)),
            },
            // host name with zone identifier.
            TestCase {
                input: "localhost%lo0:80",
                expected: Authority::new("localhost%lo0", Some(80)),
            },
            TestCase {
                input: "localhost%lo0:http",
                expected: Authority::new("localhost%lo0:http", None),
            },
            TestCase {
                input: "[localhost%lo0]:http",
                expected: Authority::new("[localhost%lo0]:http", None),
            },
            TestCase {
                input: "[localhost%lo0]:80",
                expected: Authority::new("localhost%lo0", Some(80)),
            },
            // IP literal
            TestCase {
                input: "127.0.0.1:http",
                expected: Authority::new("127.0.0.1:http", None),
            },
            TestCase {
                input: "127.0.0.1:80",
                expected: Authority::new("127.0.0.1", Some(80)),
            },
            TestCase {
                input: "[::1]:http",
                expected: Authority::new("[::1]:http", None),
            },
            TestCase {
                input: "[::1]:80",
                expected: Authority::new("::1", Some(80)),
            },
            // IP literal with zone identifier.
            TestCase {
                input: "[::1%lo0]:http",
                expected: Authority::new("[::1%lo0]:http", None),
            },
            TestCase {
                input: "[::1%lo0]:80",
                expected: Authority::new("::1%lo0", Some(80)),
            },
            TestCase {
                input: ":http",
                expected: Authority::new(":http", None),
            },
            TestCase {
                input: ":80",
                expected: Authority::new("", Some(80)),
            },
            TestCase {
                input: "grpc.io:",
                expected: Authority::new("grpc.io:", None),
            },
            TestCase {
                input: "127.0.0.1:",
                expected: Authority::new("127.0.0.1:", None),
            },
            TestCase {
                input: "[::1]:",
                expected: Authority::new("[::1]:", None),
            },
            TestCase {
                input: "grpc.io:https%foo",
                expected: Authority::new("grpc.io:https%foo", None),
            },
            TestCase {
                input: "grpc.io",
                expected: Authority::new("grpc.io", None),
            },
            TestCase {
                input: "127.0.0.1",
                expected: Authority::new("127.0.0.1", None),
            },
            TestCase {
                input: "[::1]",
                expected: Authority::new("[::1]", None),
            },
            TestCase {
                input: "[fe80::1%lo0]",
                expected: Authority::new("[fe80::1%lo0]", None),
            },
            TestCase {
                input: "[localhost%lo0]",
                expected: Authority::new("[localhost%lo0]", None),
            },
            TestCase {
                input: "localhost%lo0",
                expected: Authority::new("localhost%lo0", None),
            },
            TestCase {
                input: "::1",
                expected: Authority::new("::1", None),
            },
            TestCase {
                input: "fe80::1%lo0",
                expected: Authority::new("fe80::1%lo0", None),
            },
            TestCase {
                input: "fe80::1%lo0:80",
                expected: Authority::new("fe80::1%lo0:80", None),
            },
            TestCase {
                input: "[foo:bar]",
                expected: Authority::new("[foo:bar]", None),
            },
            TestCase {
                input: "[foo:bar]baz",
                expected: Authority::new("[foo:bar]baz", None),
            },
            TestCase {
                input: "[foo]bar:baz",
                expected: Authority::new("[foo]bar:baz", None),
            },
            TestCase {
                input: "[foo]:[bar]:baz",
                expected: Authority::new("[foo]:[bar]:baz", None),
            },
            TestCase {
                input: "[foo]:[bar]baz",
                expected: Authority::new("[foo]:[bar]baz", None),
            },
            TestCase {
                input: "foo[bar]:baz",
                expected: Authority::new("foo[bar]:baz", None),
            },
            TestCase {
                input: "foo]bar:baz",
                expected: Authority::new("foo]bar:baz", None),
            },
        ];

        for TestCase { input, expected } in cases {
            let auth = parse_authority(input);
            assert_eq!(auth, expected, "authority mismatch for {}", input);
        }
    }
}
