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

//! ADS worker that manages the xDS stream.
//!
//! The worker processes watcher and transport events serially. It owns resource
//! subscriptions, the cache, versions, and nonces, and dispatches
//! watcher notifications and ACK/NACK requests.
//!
//! A separate transport task owns retry state, connects, waits for backoff, and reads
//! and writes the ADS stream concurrently. Network waits and watcher processing
//! do not block the worker from registering watchers or replaying cached resources.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::client::config::{ClientConfig, ServerConfig};
use crate::client::retry::{Backoff, RetryPolicy};
use crate::client::snapshot::{AcceptedResource, RejectedUpdate, ResourceSnapshot, ResourceStatus};
use crate::client::watch::{ProcessingDone, ResourceEvent};
use crate::codec::XdsCodec;
use crate::error::{Error, Result};
use crate::message::{DiscoveryRequest, DiscoveryResponse, ErrorDetail, Node};
use crate::metrics::{self, KeyValue, MetricsRecorder};
use crate::resource::{DecodedResource, DecoderFn};
use crate::runtime::Runtime;
use crate::transport::{Transport, TransportBuilder, TransportReceiver, TransportSender};

/// Per-client A78 metric attributes (`grpc.target` + `grpc.xds.server`).
///
/// Both values are stored as `Arc<str>` so each emission clones them as a
/// cheap atomic op (via the `StringValue::RefCounted` variant) instead of
/// allocating a new `String` per attribute slot.
struct ClientAttrs {
    target: Arc<str>,
    server: Arc<str>,
}

impl ClientAttrs {
    /// Sentinel `grpc.xds.authority` value used for the unnamed top-level
    /// (non-federated) authority.
    ///
    /// Matches grpc-go's top-level placeholder.
    ///
    /// TODO: once federated bootstrap support lands, derive the authority from
    /// the resource name (`xdstp://<authority>/...`) on a per-resource basis.
    const TOP_LEVEL_AUTHORITY: &'static str = "#old";

    fn connection_attrs(&self) -> [KeyValue; 2] {
        [
            KeyValue::str(metrics::attrs::GRPC_TARGET, Arc::clone(&self.target)),
            KeyValue::str(metrics::attrs::GRPC_XDS_SERVER, Arc::clone(&self.server)),
        ]
    }

    fn type_attrs(&self, type_url: &Arc<str>) -> [KeyValue; 3] {
        [
            KeyValue::str(metrics::attrs::GRPC_TARGET, Arc::clone(&self.target)),
            KeyValue::str(metrics::attrs::GRPC_XDS_SERVER, Arc::clone(&self.server)),
            KeyValue::str(metrics::attrs::GRPC_XDS_RESOURCE_TYPE, Arc::clone(type_url)),
        ]
    }

    fn cache_state_attrs(&self, type_url: &Arc<str>, cache_state: &'static str) -> [KeyValue; 4] {
        [
            KeyValue::str(metrics::attrs::GRPC_TARGET, Arc::clone(&self.target)),
            KeyValue::str(
                metrics::attrs::GRPC_XDS_AUTHORITY,
                Self::TOP_LEVEL_AUTHORITY,
            ),
            KeyValue::str(metrics::attrs::GRPC_XDS_RESOURCE_TYPE, Arc::clone(type_url)),
            KeyValue::str(metrics::attrs::GRPC_XDS_CACHE_STATE, cache_state),
        ]
    }
}

/// Worker-side wrapper around an optional [`MetricsRecorder`] backend.
pub(crate) struct RecorderHandle {
    recorder: Option<Arc<dyn MetricsRecorder>>,
    attrs: ClientAttrs,
    /// Last-emitted `grpc.xds_client.resources` gauge value per
    /// `resource_type -> cache_state`. Used to diff against the live
    /// cache snapshot so we only push buckets whose count changed; the cache in
    /// the worker remains the single source of truth.
    resource_counts: HashMap<Arc<str>, HashMap<&'static str, i64>>,
}

impl RecorderHandle {
    pub(crate) fn new(recorder: Option<Arc<dyn MetricsRecorder>>, target: Arc<str>) -> Self {
        Self {
            recorder,
            attrs: ClientAttrs {
                target,
                server: Arc::from(""),
            },
            resource_counts: HashMap::new(),
        }
    }

    /// Update the `grpc.xds.server` attribute for subsequent emissions.
    pub(crate) fn set_server(&mut self, server: Arc<str>) {
        self.attrs.server = server;
    }

    /// `grpc.xds_client.connected` — 1 for connected, 0 for disconnected.
    fn record_connected(&self, connected: bool) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        recorder.record_gauge_i64(
            &metrics::instruments::XDS_CLIENT_CONNECTED,
            if connected { 1 } else { 0 },
            &self.attrs.connection_attrs(),
        );
    }

    /// `grpc.xds_client.server_failure` — incremented once per failed connection cycle.
    fn record_server_failure(&self) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        recorder.add_counter_u64(
            &metrics::instruments::XDS_CLIENT_SERVER_FAILURE,
            1,
            &self.attrs.connection_attrs(),
        );
    }

    /// `grpc.xds_client.resource_updates_valid` + `_invalid`, with aggregated
    /// counts from a single response.
    fn record_resource_updates(&self, type_url: &Arc<str>, valid: u64, invalid: u64) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        if valid == 0 && invalid == 0 {
            return;
        }
        let type_attrs = self.attrs.type_attrs(type_url);
        if valid > 0 {
            recorder.add_counter_u64(
                &metrics::instruments::XDS_CLIENT_RESOURCE_UPDATES_VALID,
                valid,
                &type_attrs,
            );
        }
        if invalid > 0 {
            recorder.add_counter_u64(
                &metrics::instruments::XDS_CLIENT_RESOURCE_UPDATES_INVALID,
                invalid,
                &type_attrs,
            );
        }
    }

    /// Reconcile the `grpc.xds_client.resources` gauge for `type_url` against an
    /// authoritative cache snapshot (`cache_state` label -> current count).
    ///
    /// The worker's resource cache is the single source of truth; this only
    /// diffs the snapshot against the values last emitted for `type_url` and
    /// pushes the buckets that changed. Buckets that dropped out of the snapshot
    /// are pushed as `0`, because a push gauge would otherwise retain a stale
    /// non-zero reading for a bucket that has emptied. Idempotent: calling it
    /// with an unchanged snapshot emits nothing.
    fn sync_resource_counts(&mut self, type_url: &Arc<str>, counts: &HashMap<&'static str, i64>) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        let last = self
            .resource_counts
            .entry(Arc::clone(type_url))
            .or_default();

        // New or changed buckets.
        for (&state, &count) in counts {
            if last.get(&state) != Some(&count) {
                recorder.record_gauge_i64(
                    &metrics::instruments::XDS_CLIENT_RESOURCES,
                    count,
                    &self.attrs.cache_state_attrs(type_url, state),
                );
            }
        }
        // Buckets that emptied since the last snapshot — reset to 0.
        for &state in last.keys() {
            if !counts.contains_key(&state) {
                recorder.record_gauge_i64(
                    &metrics::instruments::XDS_CLIENT_RESOURCES,
                    0,
                    &self.attrs.cache_state_attrs(type_url, state),
                );
            }
        }

        *last = counts.clone();
    }
}

/// Global counter for generating unique watcher IDs.
static NEXT_WATCHER_ID: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatcherId(u64);

impl WatcherId {
    /// Create a new unique watcher ID.
    pub fn new() -> Self {
        Self(NEXT_WATCHER_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for WatcherId {
    fn default() -> Self {
        Self::new()
    }
}

/// Represents the subscription mode for a resource type.
///
/// This enum captures the mutually exclusive subscription states:
/// - Wildcard: receive all resources of this type
/// - Named: receive only specific resources by name
#[derive(Debug, Clone, PartialEq, Eq)]
enum SubscriptionMode {
    /// Wildcard subscription - receive all resources of this type.
    /// In xDS protocol, this is represented by an empty resource_names list.
    Wildcard,
    /// Named subscription - receive only specific resources.
    /// Contains the set of resource names to subscribe to.
    Named(HashSet<String>),
}

impl SubscriptionMode {
    /// Get resource names for DiscoveryRequest.
    /// Returns empty vec for wildcard (xDS spec: empty = all resources).
    fn resource_names_for_request(&self) -> Vec<String> {
        match self {
            Self::Wildcard => Vec::new(),
            Self::Named(names) => names.iter().cloned().collect(),
        }
    }

    /// Returns true if the resource `name` is subscribed to.
    fn includes(&self, name: &str) -> bool {
        match self {
            Self::Wildcard => true,
            Self::Named(names) => names.contains(name),
        }
    }
}

/// State of a cached resource per gRFC A88.
#[derive(Debug, Clone)]
enum ResourceState {
    /// Resource has been requested but not yet received.
    Requested,
    /// Resource has been successfully received and validated.
    Received,
    /// Resource validation failed. Contains this resource's rejected update.
    NACKed(RejectedUpdate),
    /// Resource does not exist (server indicated deletion or absence).
    DoesNotExist,
}

impl ResourceState {
    /// Canonical A78 `grpc.xds.cache_state` attribute value for this state.
    ///
    /// When gRFC A88 (data error caching) is implemented, a `NACKedButCached`
    /// variant will map to `"nacked_but_cached"` here.
    fn cache_state_label(&self) -> &'static str {
        match self {
            ResourceState::Requested => "requested",
            ResourceState::Received => "acked",
            ResourceState::NACKed(_) => "nacked",
            ResourceState::DoesNotExist => "does_not_exist",
        }
    }

    fn status(&self) -> ResourceStatus {
        match self {
            ResourceState::Requested => ResourceStatus::Requested,
            ResourceState::Received => ResourceStatus::Acked,
            ResourceState::NACKed(_) => ResourceStatus::Nacked,
            ResourceState::DoesNotExist => ResourceStatus::DoesNotExist,
        }
    }
}

/// A cached resource entry.
#[derive(Debug, Clone)]
struct CachedResource {
    /// Current state of the resource.
    state: ResourceState,
    /// The decoded resource, if successfully received.
    /// None if state is Requested, NACKed, or DoesNotExist.
    resource: Option<Arc<DecodedResource>>,
    /// The last accepted version as received, for snapshots. Unlike
    /// `resource`, it is kept when the resource is later rejected or removed.
    accepted: Option<AcceptedResource>,
}

impl CachedResource {
    /// Create a new cached resource in Requested state.
    fn requested() -> Self {
        Self {
            state: ResourceState::Requested,
            resource: None,
            accepted: None,
        }
    }

    /// Create a cached resource in Received state.
    fn received(resource: Arc<DecodedResource>, accepted: AcceptedResource) -> Self {
        Self {
            state: ResourceState::Received,
            resource: Some(resource),
            accepted: Some(accepted),
        }
    }

    /// Create a cached resource in DoesNotExist state.
    fn does_not_exist(accepted: Option<AcceptedResource>) -> Self {
        Self {
            state: ResourceState::DoesNotExist,
            resource: None,
            accepted,
        }
    }

    /// Create a cached resource in NACKed state.
    fn nacked(rejected: RejectedUpdate, accepted: Option<AcceptedResource>) -> Self {
        Self {
            state: ResourceState::NACKed(rejected),
            resource: None,
            accepted,
        }
    }

    /// Returns true if the resource is in Requested state (waiting for server response).
    fn is_requested(&self) -> bool {
        matches!(self.state, ResourceState::Requested)
    }

    /// Convert cached state to a ResourceEvent for notifying watchers.
    /// Returns None if state is Requested (nothing to notify yet).
    fn to_event(&self) -> Option<ResourceEvent<DecodedResource>> {
        // Cache-dump events for new watchers do not gate flow control.
        let done = ProcessingDone::detached();
        match &self.state {
            ResourceState::Received => {
                self.resource
                    .as_ref()
                    .map(|r| ResourceEvent::ResourceChanged {
                        result: Ok(Arc::clone(r)),
                        done,
                    })
            }
            ResourceState::DoesNotExist => Some(ResourceEvent::ResourceChanged {
                result: Err(Error::ResourceDoesNotExist),
                done,
            }),
            ResourceState::NACKed(rejected) => Some(ResourceEvent::ResourceChanged {
                result: Err(Error::Validation(rejected.details.clone())),
                done,
            }),
            ResourceState::Requested => None,
        }
    }
}

/// Per-type_url state tracking.
struct TypeState {
    /// Reference-counted type URL, shared with metric attribute slots so
    /// per-emission attribute construction is a cheap.
    type_url: Arc<str>,
    /// Decoder function for this resource type.
    decoder: DecoderFn,
    /// Version from last successful response.
    version_info: String,
    /// Nonce from last response (for ACK/NACK).
    nonce: String,
    /// Active watchers for this type.
    watchers: HashMap<WatcherId, WatcherEntry>,
    /// Current subscription mode (wildcard or named resources).
    subscription: SubscriptionMode,
    /// Resource cache: name -> cached resource.
    cache: HashMap<String, CachedResource>,
    /// Whether missing resources in SotW should be treated as deleted (per A53).
    all_resources_required_in_sotw: bool,
}

impl std::fmt::Debug for TypeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeState")
            .field("type_url", &self.type_url)
            .field("decoder", &"<decoder fn>")
            .field("version_info", &self.version_info)
            .field("nonce", &self.nonce)
            .field("watchers", &self.watchers)
            .field("subscription", &self.subscription)
            .field("cache", &format!("<{} entries>", self.cache.len()))
            .field(
                "all_resources_required_in_sotw",
                &self.all_resources_required_in_sotw,
            )
            .finish()
    }
}

impl TypeState {
    fn new(type_url: Arc<str>, decoder: DecoderFn, all_resources_required_in_sotw: bool) -> Self {
        Self {
            type_url,
            decoder,
            version_info: String::new(),
            nonce: String::new(),
            watchers: HashMap::new(),
            subscription: SubscriptionMode::Named(HashSet::new()),
            cache: HashMap::new(),
            all_resources_required_in_sotw,
        }
    }

    /// Recalculate subscription mode from watchers.
    fn recalculate_subscriptions(&mut self) {
        let has_wildcard = self
            .watchers
            .values()
            .any(|entry| entry.subscription.is_wildcard());

        if has_wildcard {
            self.subscription = SubscriptionMode::Wildcard;
        } else {
            let names: HashSet<String> = self
                .watchers
                .values()
                .filter_map(|entry| match &entry.subscription {
                    WatcherSubscription::Named(name) => Some(name.clone()),
                    WatcherSubscription::Wildcard => None,
                })
                .collect();
            self.subscription = SubscriptionMode::Named(names);
        }
    }

    /// Get resource names to send in DiscoveryRequest.
    fn resource_names_for_request(&self) -> Vec<String> {
        self.subscription.resource_names_for_request()
    }

    /// Get senders for all watchers interested in a specific resource.
    fn matching_watchers(
        &self,
        name: &str,
    ) -> impl Iterator<Item = &mpsc::UnboundedSender<ResourceEvent<DecodedResource>>> {
        self.watchers
            .values()
            .filter(move |e| e.subscription.matches(name))
            .map(|e| &e.event_tx)
    }

    /// Snapshots of the subscribed resources in the cache.
    fn snapshot(&self) -> impl Iterator<Item = ResourceSnapshot> + '_ {
        self.cache
            .iter()
            .filter(|(name, _)| self.subscription.includes(name))
            .map(|(name, cached)| ResourceSnapshot {
                type_url: self.type_url.to_string(),
                name: name.clone(),
                status: cached.state.status(),
                accepted: cached.accepted.clone(),
                rejected: match &cached.state {
                    ResourceState::NACKed(rejected) => Some(rejected.clone()),
                    _ => None,
                },
            })
    }

    /// Current number of cached resources in each `grpc.xds.cache_state`, keyed
    /// by the canonical state label. States with no resources are omitted; this
    /// is the authoritative snapshot for the `grpc.xds_client.resources` gauge.
    fn resource_state_counts(&self) -> HashMap<&'static str, i64> {
        let mut counts: HashMap<&'static str, i64> = HashMap::new();
        for cached in self.cache.values() {
            *counts.entry(cached.state.cache_state_label()).or_insert(0) += 1;
        }
        counts
    }
}

/// Specifies which resources a watcher is interested in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WatcherSubscription {
    /// Wildcard subscription - receive all resources of this type.
    Wildcard,
    /// Named subscription - receive only the specified resource.
    Named(String),
}

impl WatcherSubscription {
    /// Create a subscription from a resource name.
    /// Empty string is treated as wildcard.
    fn from_name(name: String) -> Self {
        if name.is_empty() {
            Self::Wildcard
        } else {
            Self::Named(name)
        }
    }

    /// Check if this subscription matches a resource name.
    fn matches(&self, resource_name: &str) -> bool {
        match self {
            Self::Wildcard => true,
            Self::Named(name) => name == resource_name,
        }
    }

    /// Returns true if this is a wildcard subscription.
    fn is_wildcard(&self) -> bool {
        matches!(self, Self::Wildcard)
    }
}

/// Per-watcher state.
#[derive(Debug)]
struct WatcherEntry {
    /// Channel to send events to this watcher.
    event_tx: mpsc::UnboundedSender<ResourceEvent<DecodedResource>>,
    /// What resources this watcher is subscribed to.
    subscription: WatcherSubscription,
}

/// All inputs to the state actor share one FIFO message queue.
/// Commands, timer expirations, and transport events are processed in enqueue
/// order. Concurrent producers have no ordering guarantee before enqueueing.
pub(crate) enum WorkerCommand {
    Watcher(WatchEvent),
    Transport(TransportEvent),
    TransportStopped,
    /// Report the status of every subscribed resource.
    Snapshot {
        /// Channel to send the snapshot to.
        reply: oneshot::Sender<Vec<ResourceSnapshot>>,
    },
}

/// Watcher registrations, cancellations, and resource timer expirations.
pub(crate) enum WatchEvent {
    /// Subscribe to a resource.
    Watch {
        /// The type URL of the resource.
        type_url: &'static str,
        /// The resource name (empty string for wildcard subscription).
        name: String,
        /// Unique identifier for this watcher.
        watcher_id: WatcherId,
        /// Channel to send resource events to the watcher.
        event_tx: mpsc::UnboundedSender<ResourceEvent<DecodedResource>>,
        /// Decoder function for this resource type.
        decoder: DecoderFn,
        /// Whether all resources must be present in SotW responses (per A53).
        all_resources_required_in_sotw: bool,
    },
    /// Unsubscribe a watcher.
    Unwatch {
        /// The watcher to remove.
        watcher_id: WatcherId,
    },
    /// Timer expired for a resource that was never received (gRFC A57).
    ResourceTimerExpired {
        /// The type URL of the resource.
        type_url: String,
        /// The resource name.
        name: String,
        /// Identifies the timer, so cancelled expirations cannot affect its replacement.
        timer_id: u64,
    },
}

/// Events from the transport lifecycle, in Ready -> Response* -> Failed order.
/// Connection or stream setup failures emit Failed without a preceding Ready.
/// The lifecycle waits for the actor to finish processing failure before starting another
/// session, so events from different sessions cannot interleave.
pub(crate) enum TransportEvent {
    Ready {
        writes: mpsc::UnboundedSender<Bytes>,
        cancel: oneshot::Sender<()>,
    },
    Response {
        bytes: Bytes,
        done: ProcessingDone,
    },
    Failed {
        /// Reports whether this stream received a successfully decoded response.
        saw_response_tx: oneshot::Sender<bool>,
    },
}

/// The ADS worker manages the xDS stream and dispatches resources to watchers.
pub(crate) struct AdsWorker<C, R> {
    /// Codec for encoding/decoding messages.
    codec: C,
    /// Runtime for spawning tasks and sleeping.
    runtime: R,
    /// Node identification.
    node: Node,
    /// Timeout for initial resource response (gRFC A57). None = disabled.
    resource_initial_timeout: Option<Duration>,
    /// Weak sender for timer callback commands, so AdsWorker does not keep its own channel open.
    command_tx: mpsc::WeakUnboundedSender<WorkerCommand>,
    /// Receiver for watcher events (including resource timers) and transport events.
    command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    /// Per-type_url state.
    type_states: HashMap<String, TypeState>,
    /// Cancellation handles for resource timers (gRFC A57).
    /// Key is (type_url, resource_name). Dropping the sender cancels the timer.
    resource_timers: HashMap<(String, String), (u64, oneshot::Sender<()>)>,
    /// IDs continue across reconnects and subscription removal, wrapping at u64::MAX.
    next_resource_timer_id: u64,
    /// Optional backend + per-client A78 metric attributes
    /// (`grpc.target` + `grpc.xds.server`).
    recorder: RecorderHandle,
}

impl<C, R> AdsWorker<C, R>
where
    C: XdsCodec,
    R: Runtime,
{
    /// Create a new worker.
    pub(crate) fn new(
        codec: C,
        runtime: R,
        config: &ClientConfig,
        command_tx: mpsc::UnboundedSender<WorkerCommand>,
        command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
        recorder: Option<Arc<dyn MetricsRecorder>>,
    ) -> Self {
        let target: Arc<str> = Arc::from(config.target.clone().unwrap_or_default());
        Self {
            codec,
            runtime,
            node: config.node.clone(),
            resource_initial_timeout: config.resource_initial_timeout,
            command_tx: command_tx.downgrade(),
            command_rx,
            type_states: HashMap::new(),
            resource_timers: HashMap::new(),
            next_resource_timer_id: 0,
            recorder: RecorderHandle::new(recorder, target),
        }
    }

    /// Run the worker event loop.
    ///
    /// This method runs until all client handles and watchers are dropped
    /// (which closes the command channel).
    ///
    /// Serialization contract: this task alone mutates subscriptions, cached
    /// resources, versions, and nonces. Each message is handled to
    /// completion before the next message is received, including any stream
    /// cancellation caused by that message.
    ///
    /// Handlers must stay synchronous and must not block on network operations,
    /// watcher consumption, or ProcessingDone. They enqueue writes and watcher
    /// events; the transport task performs connection, retry, and I/O waits.
    pub(crate) async fn run<TB: TransportBuilder>(
        mut self,
        transport_context: TransportContext<R, TB>,
    ) {
        let server = match transport_context.servers.first() {
            Some(server) => server,
            None => return, // No servers configured
        };
        self.recorder.set_server(Arc::from(server.uri()));

        // gRFC A78 defines `grpc.xds_client.connected` to be initialized as
        // "For a given server, set to 1 when the stream is initially created."
        let mut context = StreamContext::new();
        self.recorder.record_connected(true);
        let (_shutdown, shutdown_rx) = oneshot::channel::<()>();
        let command_tx = self.command_tx.clone();
        let mut transport_task = Some(async move {
            tokio::select! {
                _ = transport_context.run(&command_tx) => {}
                // Covers setup, backoff, I/O, and a held ProcessingDone token.
                _ = shutdown_rx => {}
            }
            let _ = send_worker_command(&command_tx, WorkerCommand::TransportStopped);
        });
        // Process each event to completion before receiving the next one, so
        // subscriptions and cached resources change in a defined order.
        // Handlers enqueue requests and notifications without awaiting network
        // I/O or watcher consumption.
        while let Some(message) = self.command_rx.recv().await {
            let result = match message {
                WorkerCommand::Watcher(cmd) => {
                    let sender = context
                        .session
                        .as_ref()
                        .filter(|s| s.cancel.is_some())
                        .map(|s| &s.writes);
                    let result = self.handle_command(sender, cmd);
                    if !self.type_states.is_empty()
                        && let Some(task) = transport_task.take()
                    {
                        self.runtime.spawn(task);
                    }
                    result
                }
                WorkerCommand::Transport(event) => self.handle_transport_event(event, &mut context),
                WorkerCommand::TransportStopped => break,
                WorkerCommand::Snapshot { reply } => {
                    let _ = reply.send(self.snapshot());
                    Ok(())
                }
            };
            // Every protocol or write failure retires the stream the same way.
            if result.is_err()
                && let Some(active) = &mut context.session
            {
                drop(active.cancel.take());
                self.resource_timers.clear();
            }
        }
        if context.healthy {
            self.recorder.record_connected(false);
        }
    }

    /// Apply a lifecycle event without waiting on transport I/O.
    fn handle_transport_event(
        &mut self,
        event: TransportEvent,
        context: &mut StreamContext,
    ) -> Result<()> {
        match event {
            TransportEvent::Ready { writes, cancel } => {
                for state in self.type_states.values_mut() {
                    state.nonce.clear();
                }
                let active = ActiveStream::new(writes, cancel);
                // Reconcile subscriptions from current actor state after setup.
                let result = self.send_initial_requests(&active.writes);
                context.session = Some(active);
                result
            }
            TransportEvent::Response { bytes, done } => {
                let Some(active) = context.session.as_mut().filter(|s| s.cancel.is_some()) else {
                    // Ignore queued responses from a retired stream.
                    return Ok(());
                };
                let response = self.codec.decode_response(bytes)?;
                active.saw_response = true;
                self.record_healthy(&mut context.healthy);
                self.handle_response(&active.writes, response, done)
            }
            TransportEvent::Failed { saw_response_tx } => {
                // Dropping the handles cancels timers while the stream is unavailable.
                self.resource_timers.clear();
                let saw_response = context.session.take().is_some_and(|s| s.saw_response);
                if !saw_response {
                    self.record_unhealthy(&mut context.healthy);
                }
                let _ = saw_response_tx.send(saw_response);
                Ok(())
            }
        }
    }

    /// Record an xDS server transition to healthy (gRFC A78
    /// `grpc.xds_client.connected`). Sets the `connected` gauge to 1, but only
    /// on the unhealthy -> healthy edge, so repeated reconnect attempts during
    /// a single outage are not counted.
    fn record_healthy(&self, healthy: &mut bool) {
        if !*healthy {
            self.recorder.record_connected(true);
            *healthy = true;
        }
    }

    /// Record an xDS server transition to unhealthy (gRFC A78
    /// `grpc.xds_client.server_failure`). Increments the `server_failure`
    /// counter and drops the `connected` gauge to 0, but only on the
    /// healthy -> unhealthy edge, so repeated reconnect attempts during a single
    /// outage are not counted.
    fn record_unhealthy(&self, healthy: &mut bool) {
        if *healthy {
            self.recorder.record_server_failure();
            self.recorder.record_connected(false);
            *healthy = false;
        }
    }

    /// Send initial DiscoveryRequests for all active subscriptions.
    fn send_initial_requests(&mut self, sender: &mpsc::UnboundedSender<Bytes>) -> Result<()> {
        let type_urls: Vec<_> = self
            .type_states
            .iter()
            .filter(|(_, state)| !state.watchers.is_empty())
            .map(|(type_url, _)| type_url.clone())
            .collect();
        for type_url in type_urls {
            self.send_request(sender, &type_url)?;
        }

        Ok(())
    }

    /// Handle a command, optionally sending network requests if connected.
    ///
    /// When `sender` is `None`, only state updates are performed (disconnected mode).
    /// When `sender` is `Some`, subscription changes trigger network requests via
    /// the unbounded write channel.
    fn handle_command(
        &mut self,
        sender: Option<&mpsc::UnboundedSender<Bytes>>,
        cmd: WatchEvent,
    ) -> Result<()> {
        match cmd {
            WatchEvent::Watch {
                type_url,
                name,
                watcher_id,
                event_tx,
                decoder,
                all_resources_required_in_sotw,
            } => {
                let subscriptions_changed = self.add_watcher(
                    type_url,
                    name,
                    watcher_id,
                    event_tx,
                    decoder,
                    all_resources_required_in_sotw,
                );
                if let Some(sender) = sender {
                    if subscriptions_changed {
                        self.send_request(sender, type_url)?;
                    } else {
                        // The current subscription already covers this watch,
                        // including a named watch added under a wildcard.
                        self.start_pending_resource_timers(type_url);
                    }
                }
            }
            WatchEvent::Unwatch { watcher_id } => {
                if let Some((type_url, true)) = self.remove_watcher(watcher_id)
                    && let Some(sender) = sender
                {
                    self.send_request(sender, &type_url)?;
                }
            }
            WatchEvent::ResourceTimerExpired {
                type_url,
                name,
                timer_id,
            } => {
                self.handle_resource_timeout(&type_url, &name, timer_id);
            }
        }
        Ok(())
    }

    /// Snapshots of every subscribed resource, sorted by type URL and name.
    fn snapshot(&self) -> Vec<ResourceSnapshot> {
        let mut snapshot: Vec<ResourceSnapshot> = self
            .type_states
            .values()
            .flat_map(TypeState::snapshot)
            .collect();
        snapshot.sort_by(|a, b| (&a.type_url, &a.name).cmp(&(&b.type_url, &b.name)));
        snapshot
    }

    /// Add a watcher to the state.
    ///
    /// A named watcher receives any cached state immediately. Wildcard watchers
    /// receive subsequent updates without replaying the cache.
    /// Returns true if subscriptions changed (need to send new request to server).
    fn add_watcher(
        &mut self,
        type_url: &'static str,
        name: String,
        watcher_id: WatcherId,
        event_tx: mpsc::UnboundedSender<ResourceEvent<DecodedResource>>,
        decoder: DecoderFn,
        all_resources_required_in_sotw: bool,
    ) -> bool {
        let type_url_string = type_url.to_string();
        let type_state = self
            .type_states
            .entry(type_url_string.clone())
            .or_insert_with(|| {
                TypeState::new(Arc::from(type_url), decoder, all_resources_required_in_sotw)
            });

        let old_subscription = type_state.subscription.clone();
        let watcher_subscription = WatcherSubscription::from_name(name.clone());

        // Track newly-inserted cache entry for the resources gauge (None -> Requested).
        let mut was_new = false;

        // For named subscriptions, check cache and send cached state to new watcher.
        // For wildcard subscriptions, watchers receive updates as they come in.
        if let WatcherSubscription::Named(ref resource_name) = watcher_subscription {
            let cached = match type_state.cache.entry(resource_name.clone()) {
                Entry::Vacant(v) => {
                    was_new = true;
                    v.insert(CachedResource::requested())
                }
                Entry::Occupied(o) => o.into_mut(),
            };

            if let Some(event) = cached.to_event() {
                // Enqueue cached state without waiting for the watcher.
                let _ = event_tx.send(event);
            }
        }

        type_state.watchers.insert(
            watcher_id,
            WatcherEntry {
                event_tx,
                subscription: watcher_subscription,
            },
        );
        type_state.recalculate_subscriptions();

        let subscriptions_changed = type_state.subscription != old_subscription;

        // Reconcile the resources gauge from the updated cache.
        if was_new {
            let counts = type_state.resource_state_counts();
            self.recorder
                .sync_resource_counts(&type_state.type_url, &counts);
        }

        subscriptions_changed
    }

    /// Remove a watcher from the state.
    /// Returns the type_url and whether subscriptions changed.
    fn remove_watcher(&mut self, watcher_id: WatcherId) -> Option<(String, bool)> {
        let type_url = self
            .type_states
            .iter()
            .find(|(_, state)| state.watchers.contains_key(&watcher_id))
            .map(|(url, _)| url.clone())?;

        let type_state = self.type_states.get_mut(&type_url)?;

        let old_subscription = type_state.subscription.clone();

        type_state.watchers.remove(&watcher_id);
        type_state.recalculate_subscriptions();

        let subscriptions_changed = type_state.subscription != old_subscription;

        if type_state.watchers.is_empty() {
            let type_url_arc = Arc::clone(&type_state.type_url);
            self.type_states.remove(&type_url);
            // The type is gone — reset all of its resource buckets to zero.
            self.recorder
                .sync_resource_counts(&type_url_arc, &HashMap::new());
            // Cancel all pending resource timers for this type.
            self.resource_timers.retain(|key, _| key.0 != type_url);
        }

        Some((type_url, subscriptions_changed))
    }

    /// Send a DiscoveryRequest for a type to the unbounded write channel.
    fn send_request(
        &mut self,
        sender: &mpsc::UnboundedSender<Bytes>,
        type_url: &str,
    ) -> Result<()> {
        let type_state = match self.type_states.get(type_url) {
            Some(s) => s,
            None => return Ok(()),
        };

        let resource_names = type_state.resource_names_for_request();
        let request = DiscoveryRequest {
            node: &self.node,
            type_url,
            resource_names: &resource_names,
            version_info: &type_state.version_info,
            response_nonce: &type_state.nonce,
            error_detail: None,
        };

        let bytes = self.codec.encode_request(&request)?;
        sender.send(bytes).map_err(|_| Error::StreamClosed)?;
        self.start_pending_resource_timers(type_url);
        Ok(())
    }

    fn start_pending_resource_timers(&mut self, type_url: &str) {
        // Start timers after queuing subscriptions following new_stream().
        // Its completion is our readiness proxy, not proof of channel connectivity
        // or stream dispatch as required by A57. Reconnect uses this same path.
        let Some(timeout) = self.resource_initial_timeout else {
            return;
        };
        let Some(type_state) = self.type_states.get(type_url) else {
            return;
        };
        let pending: Vec<_> = type_state
            .watchers
            .values()
            .filter_map(|watcher| match &watcher.subscription {
                WatcherSubscription::Named(name)
                    if type_state.cache.get(name).is_some_and(|c| c.is_requested()) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        for name in pending {
            self.start_resource_timer(type_url, name, timeout);
        }
    }

    /// Handle a response from the server. Problems with `response` will be handled directly, and
    /// not cause an `Err` return.
    ///
    /// Implements partial success per gRFC A46: valid resources are accepted even
    /// if some resources in the response fail validation. Each resource is processed
    /// independently:
    /// - Valid resources are cached and dispatched to watchers
    /// - Invalid resources are cached as NACKed and errors sent to specific watchers
    /// - Missing resources (for types with ALL_RESOURCES_REQUIRED_IN_SOTW) are marked deleted
    ///
    /// Cache/state updates, notification enqueueing, and the ACK/NACK form one
    /// synchronous actor turn. Watcher queues are unbounded, so a slow watcher
    /// cannot suspend this turn. `ProcessingDone` gates only the next stream read.
    fn handle_response(
        &mut self,
        sender: &mpsc::UnboundedSender<Bytes>,
        response: DiscoveryResponse,
        done: ProcessingDone,
    ) -> Result<()> {
        let type_url = response.type_url.clone();

        let (type_url_arc, decoder) = match self.type_states.get(&type_url) {
            Some(s) => (Arc::clone(&s.type_url), &s.decoder),
            None => {
                return Ok(());
            }
        };

        // Decode all resources, tracking valid and invalid separately.
        // Per A46, we accept valid resources even if some fail validation.
        // Per A88, we categorize errors:
        // - top_level_errors: deserialization failures where name cannot be extracted
        // - per_resource_errors: validation failures where name is known
        let now = SystemTime::now();
        let mut valid_resources: Vec<(DecodedResource, AcceptedResource)> = Vec::new();
        let mut top_level_errors: Vec<String> = Vec::new();
        let mut per_resource_errors: Vec<(String, String)> = Vec::new(); // (name, error)

        for resource_any in &response.resources {
            match decoder(resource_any.value.clone()) {
                crate::resource::DecodeResult::Success { resource, .. } => {
                    let accepted = AcceptedResource::new(
                        response.version_info.clone(),
                        resource_any.clone(),
                        now,
                    );
                    valid_resources.push((resource, accepted));
                }
                crate::resource::DecodeResult::ResourceError { name, error } => {
                    per_resource_errors.push((name, error.to_string()));
                }
                crate::resource::DecodeResult::TopLevelError(error) => {
                    top_level_errors.push(error.to_string());
                }
            }
        }

        // Emit A78 resource_updates_valid/invalid counters once per response with
        // aggregated counts (equivalent to per-resource increments in any backend).
        let valid_count = valid_resources.len() as u64;
        let invalid_count = (top_level_errors.len() + per_resource_errors.len()) as u64;
        self.recorder
            .record_resource_updates(&type_url_arc, valid_count, invalid_count);

        if let Some(type_state) = self.type_states.get_mut(&type_url) {
            type_state.nonce = response.nonce.clone();
        }

        let received_names: HashSet<String> = valid_resources
            .iter()
            .map(|(r, _)| r.name().to_string())
            .collect();

        let nack_error = nack_error_message(&top_level_errors, &per_resource_errors);

        self.dispatch_resources(&type_url, valid_resources, &done);

        // Only notify watchers for per-resource errors (where we know the name).
        // Top-level errors have no associated name, so no watcher to notify.
        for (resource_name, error) in per_resource_errors {
            let rejected = RejectedUpdate::new(response.version_info.clone(), error, now);
            self.notify_resource_error(&type_url, &resource_name, rejected, &done);
        }

        // Detect deleted resources (per A53):
        // For resource types with ALL_RESOURCES_REQUIRED_IN_SOTW = true,
        // any previously-received resource not in this response is deleted.
        self.detect_deleted_resources(&type_url, &received_names, &done);

        match nack_error {
            None => {
                // Only update version on ACK; NACK must keep the old version so the
                // server knows which version the client is still running.
                if let Some(ts) = self.type_states.get_mut(&type_url) {
                    ts.version_info = response.version_info.clone();
                }
                self.send_ack(sender, &response)?;
            }
            Some(error) => self.send_nack(sender, &response, error)?,
        }

        Ok(())
    }

    /// Update the cache from decoded resources and notify matching watchers.
    ///
    /// Delivered events share the response's `ProcessingDone` signal, which
    /// gates reading the next response (ADS flow control).
    fn dispatch_resources(
        &mut self,
        type_url: &str,
        resources: Vec<(DecodedResource, AcceptedResource)>,
        done: &ProcessingDone,
    ) {
        let Some(type_state) = self.type_states.get_mut(type_url) else {
            return;
        };

        for (resource, accepted) in resources {
            let resource_name = resource.name().to_string();
            let resource = Arc::new(resource);
            type_state.cache.insert(
                resource_name.clone(),
                CachedResource::received(Arc::clone(&resource), accepted),
            );
            // Cancel the initial resource timer (gRFC A57).
            self.resource_timers
                .remove(&(type_url.to_string(), resource_name.clone()));

            for event_tx in type_state.matching_watchers(&resource_name) {
                let _ = event_tx.send(ResourceEvent::ResourceChanged {
                    result: Ok(Arc::clone(&resource)),
                    done: done.share(),
                });
            }
        }
        let counts = type_state.resource_state_counts();
        self.recorder
            .sync_resource_counts(&type_state.type_url, &counts);
    }

    /// Send validation-error notifications for a specific resource.
    ///
    /// Per gRFC A46/A88, errors are routed only to watchers interested in
    /// that specific resource (plus wildcard watchers).
    fn notify_resource_error(
        &mut self,
        type_url: &str,
        resource_name: &str,
        rejected: RejectedUpdate,
        done: &ProcessingDone,
    ) {
        let type_state = match self.type_states.get_mut(type_url) {
            Some(s) => s,
            None => return,
        };

        let error = rejected.details.clone();
        let accepted = type_state
            .cache
            .remove(resource_name)
            .and_then(|c| c.accepted);
        type_state.cache.insert(
            resource_name.to_string(),
            CachedResource::nacked(rejected, accepted),
        );
        let counts = type_state.resource_state_counts();
        self.recorder
            .sync_resource_counts(&type_state.type_url, &counts);

        // Cancel the resource timer (gRFC A57).
        self.resource_timers
            .remove(&(type_url.to_string(), resource_name.to_string()));

        for event_tx in type_state.matching_watchers(resource_name) {
            let event = ResourceEvent::ResourceChanged {
                result: Err(Error::Validation(error.clone())),
                done: done.share(),
            };
            let _ = event_tx.send(event);
        }
    }

    /// Detect resources that were deleted (present in cache but not in response)
    /// and notify matching watchers.
    ///
    /// Per gRFC A53, for resource types with ALL_RESOURCES_REQUIRED_IN_SOTW = true,
    /// if a previously-received resource is absent from a new SotW response,
    /// it is treated as deleted.
    fn detect_deleted_resources(
        &mut self,
        type_url: &str,
        received_names: &HashSet<String>,
        done: &ProcessingDone,
    ) {
        let type_state = match self.type_states.get_mut(type_url) {
            Some(s) => s,
            None => return,
        };

        if !type_state.all_resources_required_in_sotw {
            return;
        }

        let deleted_names: Vec<String> = type_state
            .cache
            .iter()
            .filter(|(name, cached)| {
                matches!(cached.state, ResourceState::Received) && !received_names.contains(*name)
            })
            .map(|(name, _)| name.clone())
            .collect();

        for name in deleted_names {
            let accepted = type_state.cache.remove(&name).and_then(|c| c.accepted);
            type_state
                .cache
                .insert(name.clone(), CachedResource::does_not_exist(accepted));

            for event_tx in type_state.matching_watchers(&name) {
                let event = ResourceEvent::ResourceChanged {
                    result: Err(Error::ResourceDoesNotExist),
                    done: done.share(),
                };
                let _ = event_tx.send(event);
            }
        }

        // Reconcile the resources gauge once from the updated cache.
        let counts = type_state.resource_state_counts();
        self.recorder
            .sync_resource_counts(&type_state.type_url, &counts);
    }

    /// Send an ACK for a response to the unbounded write channel.
    fn send_ack(
        &self,
        sender: &mpsc::UnboundedSender<Bytes>,
        response: &DiscoveryResponse,
    ) -> Result<()> {
        let type_state = match self.type_states.get(&response.type_url) {
            Some(s) => s,
            None => return Ok(()),
        };

        let resource_names = type_state.resource_names_for_request();
        let request = DiscoveryRequest {
            node: &self.node,
            type_url: &response.type_url,
            resource_names: &resource_names,
            version_info: &response.version_info,
            response_nonce: &response.nonce,
            error_detail: None,
        };

        let bytes = self.codec.encode_request(&request)?;
        sender.send(bytes).map_err(|_| Error::StreamClosed)?;
        Ok(())
    }

    /// Send a NACK for a response to the unbounded write channel.
    fn send_nack(
        &self,
        sender: &mpsc::UnboundedSender<Bytes>,
        response: &DiscoveryResponse,
        error_message: String,
    ) -> Result<()> {
        let type_state = match self.type_states.get(&response.type_url) {
            Some(s) => s,
            None => return Ok(()),
        };

        let resource_names = type_state.resource_names_for_request();
        let request = DiscoveryRequest {
            node: &self.node,
            type_url: &response.type_url,
            resource_names: &resource_names,
            version_info: &type_state.version_info, // Keep old version for NACK
            response_nonce: &response.nonce,
            error_detail: Some(ErrorDetail {
                code: 3, // INVALID_ARGUMENT
                message: error_message,
            }),
        };

        let bytes = self.codec.encode_request(&request)?;
        sender.send(bytes).map_err(|_| Error::StreamClosed)?;
        Ok(())
    }

    /// Start a timer for a resource in Requested state (gRFC A57).
    ///
    /// If a timer is already running for this resource, this is a no-op to
    /// preserve the original timeout deadline per A57.
    ///
    /// When the timer fires, it sends a `ResourceTimerExpired` command.
    /// The handler checks if the resource is still in Requested state before acting.
    fn start_resource_timer(&mut self, type_url: &str, name: String, timeout: Duration) {
        let key = (type_url.to_string(), name.clone());

        // Don't reset an existing timer — A57 says timeout starts on first request.
        if self.resource_timers.contains_key(&key) {
            return;
        }

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let timer_id = self.next_resource_timer_id;
        self.next_resource_timer_id = timer_id.wrapping_add(1);
        let type_url_owned = type_url.to_string();
        let command_tx = self.command_tx.clone();
        let runtime = self.runtime.clone();

        self.runtime.spawn(async move {
            tokio::select! {
                _ = runtime.sleep(timeout) => {
                    let _ = send_worker_command(
                        &command_tx,
                        WorkerCommand::Watcher(WatchEvent::ResourceTimerExpired {
                            type_url: type_url_owned,
                            name,
                            timer_id,
                        }),
                    );
                }
                _ = cancel_rx => {}
            }
        });

        self.resource_timers.insert(key, (timer_id, cancel_tx));
    }

    /// Handle a resource timer expiration (gRFC A57).
    ///
    /// If the resource is still in Requested state, marks it as DoesNotExist
    /// and notifies all watchers interested in this resource.
    fn handle_resource_timeout(&mut self, type_url: &str, name: &str, timer_id: u64) {
        // Cancellation cannot retract an expiration already in the command queue.
        let key = (type_url.to_string(), name.to_string());
        let Entry::Occupied(timer) = self.resource_timers.entry(key) else {
            return;
        };
        if timer.get().0 != timer_id {
            return;
        }
        timer.remove();

        let type_state = match self.type_states.get_mut(type_url) {
            Some(s) => s,
            None => return,
        };

        let is_pending = type_state
            .cache
            .get(name)
            .map(|c| c.is_requested())
            .unwrap_or(true);

        if !is_pending {
            return;
        }

        type_state
            .cache
            .insert(name.to_string(), CachedResource::does_not_exist(None));
        let counts = type_state.resource_state_counts();
        self.recorder
            .sync_resource_counts(&type_state.type_url, &counts);

        for event_tx in type_state.matching_watchers(name) {
            let event = ResourceEvent::ResourceChanged {
                result: Err(Error::ResourceDoesNotExist),
                done: ProcessingDone::detached(),
            };
            let _ = event_tx.send(event);
        }
    }
}

/// Runtime, transport builder, and configuration owned by the lifecycle task.
pub(crate) struct TransportContext<R, TB> {
    runtime: R,
    builder: TB,
    servers: Vec<ServerConfig>,
    retry_policy: RetryPolicy,
}

impl<R: Runtime, TB: TransportBuilder> TransportContext<R, TB> {
    pub(crate) fn new(
        runtime: R,
        builder: TB,
        servers: Vec<ServerConfig>,
        retry_policy: RetryPolicy,
    ) -> Self {
        Self {
            runtime,
            builder,
            servers,
            retry_policy,
        }
    }

    /// One long-lived lifecycle task. It owns transport handles and waits, but
    /// never reads or mutates resource, subscription, version, or nonce state.
    async fn run(self, command_tx: &mpsc::WeakUnboundedSender<WorkerCommand>) {
        // Future extension (gRFC A71): Try servers in priority order with fallback.
        let Some(server) = self.servers.first() else {
            return;
        };
        let mut backoff = Backoff::new(self.retry_policy);
        loop {
            if let Ok(transport) = self.builder.build(server).await
                && let Ok((tx, rx)) = transport.new_stream().await
            {
                let (writes, write_rx) = mpsc::unbounded_channel();
                let (cancel, cancelled) = oneshot::channel();
                if send_worker_command(
                    command_tx,
                    WorkerCommand::Transport(TransportEvent::Ready { writes, cancel }),
                )
                .is_err()
                {
                    return;
                }
                // Read and write futures live only for this session. Cancelling one
                // disposes of both; an old completion token cannot resume new I/O.
                tokio::select! {
                    _ = Self::run_stream::<TB::Transport>(tx, rx, write_rx, command_tx) => {}
                    _ = cancelled => {}
                }
            }

            let (saw_response_tx, saw_response_rx) = oneshot::channel();
            if send_worker_command(
                command_tx,
                WorkerCommand::Transport(TransportEvent::Failed { saw_response_tx }),
            )
            .is_err()
            {
                return;
            }
            // Only the actor can distinguish decoded responses from invalid bytes.
            let Ok(saw_response) = saw_response_rx.await else {
                return;
            };
            if saw_response {
                backoff.reset();
            }
            let Some(delay) = backoff.next_backoff() else {
                return;
            };
            self.runtime.sleep(delay).await;
        }
    }

    /// Concurrent stream I/O within the lifecycle task. ProcessingDone gates reads
    /// only; writes keep running while watchers apply a response.
    async fn run_stream<T: Transport>(
        mut tx: T::Sender,
        mut rx: T::Receiver,
        mut writes: mpsc::UnboundedReceiver<Bytes>,
        command_tx: &mpsc::WeakUnboundedSender<WorkerCommand>,
    ) {
        let write_loop = async {
            while let Some(bytes) = writes.recv().await {
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        };
        let read_loop = async {
            while let Ok(Some(bytes)) = rx.recv().await {
                let (done, processed) = ProcessingDone::channel();
                if send_worker_command(
                    command_tx,
                    WorkerCommand::Transport(TransportEvent::Response { bytes, done }),
                )
                .is_err()
                {
                    break;
                }
                let _ = processed.await;
            }
        };
        tokio::select! {
            _ = write_loop => {}
            _ = read_loop => {}
        }
    }
}

/// Tracks server health across reconnects and the current stream, if any.
struct StreamContext {
    healthy: bool,
    session: Option<ActiveStream>,
}

impl StreamContext {
    fn new() -> Self {
        Self {
            healthy: true,
            session: None,
        }
    }
}

/// Holds the current stream's request queue, cancellation signal, and response history.
/// Cached resources stay in the worker so retiring this stream does not discard them.
struct ActiveStream {
    writes: mpsc::UnboundedSender<Bytes>,
    cancel: Option<oneshot::Sender<()>>,
    saw_response: bool,
}

impl ActiveStream {
    fn new(writes: mpsc::UnboundedSender<Bytes>, cancel: oneshot::Sender<()>) -> Self {
        Self {
            writes,
            cancel: Some(cancel),
            saw_response: false,
        }
    }
}

// Use a weak sender so background tasks don't prevent worker shutdown
// after all client handles and watchers are dropped.
fn send_worker_command(
    command_tx: &mpsc::WeakUnboundedSender<WorkerCommand>,
    message: WorkerCommand,
) -> Result<()> {
    command_tx
        .upgrade()
        .ok_or(Error::StreamClosed)?
        .send(message)
        .map_err(|_| Error::StreamClosed)
}

/// The NACK error for a response, or `None` if every resource was valid.
fn nack_error_message(
    top_level_errors: &[String],
    per_resource_errors: &[(String, String)],
) -> Option<String> {
    let mut error_parts = Vec::new();

    if !top_level_errors.is_empty() {
        error_parts.push(format!("top level errors: {}", top_level_errors.join("; ")));
    }

    if !per_resource_errors.is_empty() {
        let per_resource_msg = per_resource_errors
            .iter()
            .map(|(name, err)| format!("{name}: {err}"))
            .collect::<Vec<_>>()
            .join("; ");
        error_parts.push(per_resource_msg);
    }

    (!error_parts.is_empty()).then(|| error_parts.join("; "))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use bytes::Bytes;

    use super::*;
    use crate::client::config::{ClientConfig, ServerConfig};
    use crate::client::watch::{ResourceEvent, ResourceWatcher};
    use crate::codec::XdsCodec;
    use crate::error::Result;
    use crate::message::{DiscoveryRequest, DiscoveryResponse, Node, ResourceAny};
    use crate::resource::{Resource, TypeUrl};
    use crate::runtime::tokio::TokioRuntime;
    use crate::transport::TransportBuilder;
    use crate::transport::mock::{MockServer, MockTransport, mock_transport};
    use crate::{Error, XdsClient};

    /// Captures every measurement so tests can assert on the call sequence.
    #[derive(Default)]
    struct CapturingRecorder {
        events: Mutex<Vec<Recorded>>,
    }

    #[derive(Debug, PartialEq)]
    struct Recorded {
        instrument: &'static str,
        kind: Measurement,
        attrs: Vec<(&'static str, String)>,
    }

    #[derive(Debug, PartialEq)]
    enum Measurement {
        CounterU64(u64),
        UpDownI64(i64),
        Gauge(i64),
    }

    impl CapturingRecorder {
        fn take(&self) -> Vec<Recorded> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    fn stringify(attrs: &[KeyValue]) -> Vec<(&'static str, String)> {
        attrs
            .iter()
            .map(|kv| {
                let v = match &kv.value {
                    metrics::Value::Bool(b) => b.to_string(),
                    metrics::Value::Int(i) => i.to_string(),
                    metrics::Value::F64(f) => f.to_string(),
                    metrics::Value::Str(s) => s.to_string(),
                };
                (kv.key, v)
            })
            .collect()
    }

    impl MetricsRecorder for CapturingRecorder {
        fn add_counter_u64(
            &self,
            instrument: &'static metrics::Instrument,
            value: u64,
            attrs: &[KeyValue],
        ) {
            self.events.lock().unwrap().push(Recorded {
                instrument: instrument.name,
                kind: Measurement::CounterU64(value),
                attrs: stringify(attrs),
            });
        }

        fn add_up_down_counter_i64(
            &self,
            instrument: &'static metrics::Instrument,
            value: i64,
            attrs: &[KeyValue],
        ) {
            self.events.lock().unwrap().push(Recorded {
                instrument: instrument.name,
                kind: Measurement::UpDownI64(value),
                attrs: stringify(attrs),
            });
        }

        fn record_histogram_f64(&self, _: &'static metrics::Instrument, _: f64, _: &[KeyValue]) {
            unreachable!("worker emits no histograms");
        }

        fn record_gauge_i64(
            &self,
            instrument: &'static metrics::Instrument,
            value: i64,
            attrs: &[KeyValue],
        ) {
            self.events.lock().unwrap().push(Recorded {
                instrument: instrument.name,
                kind: Measurement::Gauge(value),
                attrs: stringify(attrs),
            });
        }
    }

    fn attr<'a>(rec: &'a Recorded, key: &str) -> Option<&'a str> {
        rec.attrs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Build a [`RecorderHandle`] backed by a [`CapturingRecorder`], wired
    /// with the canonical test attributes used by the transition tests.
    fn test_handle() -> (Arc<CapturingRecorder>, RecorderHandle) {
        let recorder = Arc::new(CapturingRecorder::default());
        let dyn_recorder: Arc<dyn MetricsRecorder> = recorder.clone();
        let mut handle = RecorderHandle::new(Some(dyn_recorder), Arc::from("xds:///my-service"));
        handle.set_server(Arc::from("xds.example.com:443"));
        (recorder, handle)
    }

    fn test_type_url() -> Arc<str> {
        Arc::from("envoy.config.listener.v3.Listener")
    }

    /// Value of the `resources` gauge emitted for a given `cache_state`, if any.
    fn gauge_for(events: &[Recorded], cache_state: &str) -> Option<i64> {
        events.iter().find_map(|e| {
            if attr(e, "grpc.xds.cache_state") == Some(cache_state) {
                match e.kind {
                    Measurement::Gauge(v) => Some(v),
                    _ => None,
                }
            } else {
                None
            }
        })
    }

    #[test]
    fn first_sync_emits_each_bucket_with_attrs() {
        let (recorder, mut handle) = test_handle();
        let type_url = test_type_url();
        let counts: HashMap<&'static str, i64> = HashMap::from([("acked", 2), ("requested", 1)]);
        handle.sync_resource_counts(&type_url, &counts);

        let events = recorder.take();
        assert_eq!(events.len(), 2);
        assert_eq!(gauge_for(&events, "acked"), Some(2));
        assert_eq!(gauge_for(&events, "requested"), Some(1));

        let acked = events
            .iter()
            .find(|e| attr(e, "grpc.xds.cache_state") == Some("acked"))
            .expect("acked bucket emitted");
        assert_eq!(acked.instrument, "grpc.xds_client.resources");
        assert_eq!(
            attr(acked, "grpc.xds.resource_type"),
            Some("envoy.config.listener.v3.Listener")
        );
        assert_eq!(attr(acked, "grpc.target"), Some("xds:///my-service"));
        assert_eq!(attr(acked, "grpc.xds.authority"), Some("#old"));
        assert_eq!(attr(acked, "grpc.xds.server"), None);
    }

    #[test]
    fn unchanged_sync_is_idempotent() {
        let (recorder, mut handle) = test_handle();
        let type_url = test_type_url();
        let counts: HashMap<&'static str, i64> = HashMap::from([("acked", 2)]);
        handle.sync_resource_counts(&type_url, &counts);
        let _ = recorder.take();

        handle.sync_resource_counts(&type_url, &counts);
        assert!(recorder.take().is_empty());
    }

    #[test]
    fn sync_emits_only_changed_buckets() {
        let (recorder, mut handle) = test_handle();
        let type_url = test_type_url();
        handle.sync_resource_counts(&type_url, &HashMap::from([("acked", 2)]));
        let _ = recorder.take();

        // `acked` drops to 1 and a new `nacked` bucket appears.
        handle.sync_resource_counts(&type_url, &HashMap::from([("acked", 1), ("nacked", 1)]));

        let events = recorder.take();
        assert_eq!(events.len(), 2);
        assert_eq!(gauge_for(&events, "acked"), Some(1));
        assert_eq!(gauge_for(&events, "nacked"), Some(1));
    }

    #[test]
    fn emptied_bucket_is_reset_to_zero() {
        let (recorder, mut handle) = test_handle();
        let type_url = test_type_url();
        handle.sync_resource_counts(&type_url, &HashMap::from([("acked", 1)]));
        let _ = recorder.take();

        // The whole type empties (e.g. all resources removed).
        handle.sync_resource_counts(&type_url, &HashMap::new());

        let events = recorder.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].instrument, "grpc.xds_client.resources");
        assert_eq!(gauge_for(&events, "acked"), Some(0));
    }

    const TEST_TYPE_URL: &str = "type.googleapis.com/test.Resource";
    const SOTW_TYPE_URL: &str = "type.googleapis.com/test.SotwResource";
    const VERSIONED_TYPE_URL: &str = "type.googleapis.com/test.VersionedResource";

    /// Minimal resource: the message is the resource name itself. Names
    /// starting with `bad` fail validation (a per-resource error, per A46).
    #[derive(Debug, Clone)]
    struct TestResource;

    impl Resource for TestResource {
        type Message = String;
        const TYPE_URL: TypeUrl = TypeUrl::new(TEST_TYPE_URL);
        const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

        fn deserialize(bytes: Bytes) -> Result<Self::Message> {
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Validation(e.to_string()))
        }

        fn name(message: &Self::Message) -> &str {
            message
        }

        fn validate(message: Self::Message) -> Result<Self> {
            if message.starts_with("bad") {
                return Err(Error::Validation("bad resource".to_string()));
            }
            Ok(Self)
        }
    }

    /// A resource sent as `name` or `name#bad`. Unlike [`TestResource`],
    /// validity depends on the content rather than the name, so one resource
    /// can be accepted, rejected, and accepted again. The validation error
    /// names the resource, so each rejection is distinct.
    #[derive(Debug, Clone)]
    struct VersionedResource;

    impl Resource for VersionedResource {
        type Message = String;
        const TYPE_URL: TypeUrl = TypeUrl::new(VERSIONED_TYPE_URL);
        const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

        fn deserialize(bytes: Bytes) -> Result<Self::Message> {
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Validation(e.to_string()))
        }

        fn name(message: &Self::Message) -> &str {
            message.split('#').next().unwrap_or_default()
        }

        fn validate(message: Self::Message) -> Result<Self> {
            if let Some(name) = message.strip_suffix("#bad") {
                return Err(Error::Validation(format!("{name} is bad")));
            }
            Ok(Self)
        }
    }

    /// Like [`TestResource`] but with `ALL_RESOURCES_REQUIRED_IN_SOTW`, so
    /// resources missing from a response are treated as deleted (gRFC A53).
    #[derive(Debug, Clone)]
    struct SotwResource;

    impl Resource for SotwResource {
        type Message = String;
        const TYPE_URL: TypeUrl = TypeUrl::new(SOTW_TYPE_URL);
        const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = true;

        fn deserialize(bytes: Bytes) -> Result<Self::Message> {
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Validation(e.to_string()))
        }

        fn name(message: &Self::Message) -> &str {
            message
        }

        fn validate(_message: Self::Message) -> Result<Self> {
            Ok(Self)
        }
    }

    /// Line-based codec: `type_url \n version \n nonce \n name,name,...`.
    struct FakeCodec;

    impl XdsCodec for FakeCodec {
        fn encode_request(&self, request: &DiscoveryRequest<'_>) -> Result<Bytes> {
            Ok(Bytes::from(format!(
                "{}\n{}\n{}\n{}",
                request.type_url,
                request.version_info,
                request.response_nonce,
                request.resource_names.join(",")
            )))
        }

        fn decode_response(&self, bytes: Bytes) -> Result<DiscoveryResponse> {
            let text =
                String::from_utf8(bytes.to_vec()).map_err(|e| Error::Validation(e.to_string()))?;
            let mut lines = text.split('\n');
            let type_url = lines.next().unwrap_or_default().to_string();
            let version_info = lines.next().unwrap_or_default().to_string();
            let nonce = lines.next().unwrap_or_default().to_string();
            let resources = lines
                .next()
                .unwrap_or_default()
                .split(',')
                .filter(|n| !n.is_empty())
                .map(|name| ResourceAny {
                    type_url: type_url.clone(),
                    value: Bytes::from(name.to_string()),
                })
                .collect();
            Ok(DiscoveryResponse {
                version_info,
                resources,
                type_url,
                nonce,
            })
        }
    }

    fn response_for(type_url: &str, version: &str, nonce: &str, names: &[&str]) -> Bytes {
        Bytes::from(format!(
            "{type_url}\n{version}\n{nonce}\n{}",
            names.join(",")
        ))
    }

    fn response(version: &str, nonce: &str, names: &[&str]) -> Bytes {
        response_for(TEST_TYPE_URL, version, nonce, names)
    }

    fn sotw_response(version: &str, nonce: &str, names: &[&str]) -> Bytes {
        response_for(SOTW_TYPE_URL, version, nonce, names)
    }

    /// (version_info, response_nonce) of an encoded request.
    fn parse_request(bytes: &Bytes) -> (String, String) {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let mut lines = text.split('\n');
        let _type_url = lines.next().unwrap_or_default();
        let version = lines.next().unwrap_or_default().to_string();
        let nonce = lines.next().unwrap_or_default().to_string();
        (version, nonce)
    }

    /// Client watching `res-0` (of resource type `T`) with an established
    /// mock stream, its initial request already drained.
    async fn connected_client_for<T: Resource>() -> (XdsClient, ResourceWatcher<T>, MockServer) {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();

        let watcher = client.watch::<T>("res-0").await;
        let mut server = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream")
            .expect("transport dropped");
        let _initial = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("timed out waiting for initial request")
            .expect("stream closed");
        (client, watcher, server)
    }

    async fn connected_client() -> (XdsClient, ResourceWatcher<TestResource>, MockServer) {
        connected_client_for::<TestResource>().await
    }

    #[tokio::test(start_paused = true)]
    async fn response_before_watch_is_replayed_from_cache() {
        let (client, mut watcher_a, _server) = connected_client().await;
        let (done, processed) = ProcessingDone::channel();
        client
            .command_tx
            .send(WorkerCommand::Transport(TransportEvent::Response {
                bytes: response("1", "nonce", &["res-0"]),
                done,
            }))
            .unwrap();
        // Enqueue both inputs before yielding to the actor. The response must
        // populate the cache before this watch is registered.
        let mut watcher_b = client.watch::<TestResource>("res-0").await;
        let (cached, cached_done) = next_changed(&mut watcher_b).await;
        let (original, original_done) = next_changed(&mut watcher_a).await;
        assert!(Arc::ptr_eq(&cached.unwrap(), &original.unwrap()));
        drop(original_done);
        // Holding the cached token cannot hold the earlier response's signal.
        tokio::time::timeout(Duration::from_millis(100), processed)
            .await
            .expect("later watch was included in the earlier response")
            .unwrap_err();
        drop(cached_done);
    }

    #[derive(Clone, Copy)]
    enum ReconnectPhase {
        Build,
        Stream,
        #[cfg(feature = "codegen-prost")]
        Backoff,
    }

    struct GatedBuilder {
        inner: crate::transport::mock::MockTransportBuilder,
        phase: ReconnectPhase,
        gates: mpsc::UnboundedSender<oneshot::Sender<()>>,
    }

    struct GatedTransport {
        inner: MockTransport,
        phase: ReconnectPhase,
        gates: mpsc::UnboundedSender<oneshot::Sender<()>>,
    }

    async fn connection_gate(gates: &mpsc::UnboundedSender<oneshot::Sender<()>>) -> Result<()> {
        let (release, wait) = oneshot::channel();
        gates.send(release).map_err(|_| Error::StreamClosed)?;
        wait.await.map_err(|_| Error::StreamClosed)
    }

    impl TransportBuilder for GatedBuilder {
        type Transport = GatedTransport;

        async fn build(&self, server: &ServerConfig) -> Result<Self::Transport> {
            if !matches!(self.phase, ReconnectPhase::Stream) {
                connection_gate(&self.gates).await?;
            }
            Ok(GatedTransport {
                inner: self.inner.build(server).await?,
                phase: self.phase,
                gates: self.gates.clone(),
            })
        }
    }

    impl Transport for GatedTransport {
        type Sender = <MockTransport as Transport>::Sender;
        type Receiver = <MockTransport as Transport>::Receiver;

        async fn new_stream(&self) -> Result<(Self::Sender, Self::Receiver)> {
            if matches!(self.phase, ReconnectPhase::Stream) {
                connection_gate(&self.gates).await?;
            }
            self.inner.new_stream().await
        }
    }

    #[derive(Clone)]
    struct ObservedRuntime {
        sleeps: mpsc::UnboundedSender<Duration>,
    }

    impl Runtime for ObservedRuntime {
        fn spawn<F>(&self, future: F)
        where
            F: std::future::Future<Output = ()> + Send + 'static,
        {
            TokioRuntime.spawn(future);
        }

        async fn sleep(&self, duration: Duration) {
            let _ = self.sleeps.send(duration);
            TokioRuntime.sleep(duration).await;
        }
    }

    #[cfg(feature = "codegen-prost")]
    #[derive(Debug)]
    struct EdsResource(envoy_types::pb::envoy::config::endpoint::v3::ClusterLoadAssignment);

    #[cfg(feature = "codegen-prost")]
    impl Resource for EdsResource {
        type Message = envoy_types::pb::envoy::config::endpoint::v3::ClusterLoadAssignment;
        const TYPE_URL: TypeUrl =
            TypeUrl::new("type.googleapis.com/envoy.config.endpoint.v3.ClusterLoadAssignment");
        const ALL_RESOURCES_REQUIRED_IN_SOTW: bool = false;

        fn deserialize(bytes: Bytes) -> Result<Self::Message> {
            use prost::Message;
            Self::Message::decode(bytes).map_err(Error::Decode)
        }

        fn name(message: &Self::Message) -> &str {
            &message.cluster_name
        }

        fn validate(message: Self::Message) -> Result<Self> {
            if message.cluster_name.is_empty() {
                return Err(Error::Validation("empty EDS resource name".into()));
            }
            Ok(Self(message))
        }
    }

    #[cfg(feature = "codegen-prost")]
    async fn cached_eds_during_reconnect(phase: ReconnectPhase) {
        use envoy_types::pb::envoy::config::core::v3 as core;
        use envoy_types::pb::envoy::service::discovery::v3 as discovery;
        use envoy_types::pb::google::protobuf::{Any, UInt32Value};
        use prost::Message;

        let (inner, mut servers) = mock_transport();
        let (gates_tx, mut gates) = mpsc::unbounded_channel();
        let (sleeps_tx, mut sleeps) = mpsc::unbounded_channel();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(None)
            .with_retry_policy(crate::RetryPolicy::default().with_jitter(0.0).unwrap());
        let client = XdsClient::builder(
            config,
            GatedBuilder {
                inner,
                phase,
                gates: gates_tx,
            },
            crate::codec::prost::ProstCodec,
            ObservedRuntime { sleeps: sleeps_tx },
        )
        .build();

        let mut a = client.watch::<EdsResource>("cached-eds").await;
        gates.recv().await.unwrap().send(()).unwrap();
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        let mut assignment =
            xds_test_util::config::build_cla("cached-eds", &[("127.0.0.1".into(), 8080)]);
        let locality = &mut assignment.endpoints[0];
        locality.locality = Some(core::Locality {
            region: "region".into(),
            zone: "zone".into(),
            sub_zone: "subzone".into(),
        });
        locality.load_balancing_weight = Some(UInt32Value { value: 7 });
        locality.lb_endpoints[0].health_status = core::HealthStatus::Healthy as i32;
        locality.lb_endpoints[0].load_balancing_weight = Some(UInt32Value { value: 3 });
        let mut response = discovery::DiscoveryResponse {
            version_info: "accepted-version".into(),
            nonce: "old-nonce".into(),
            type_url: EdsResource::TYPE_URL.as_str().into(),
            resources: vec![Any {
                type_url: EdsResource::TYPE_URL.as_str().into(),
                value: assignment.encode_to_vec(),
            }],
            ..Default::default()
        };
        server
            .responses
            .send(Ok(Some(response.encode_to_vec().into())))
            .unwrap();
        let (cached, done) = next_changed(&mut a).await;
        let cached = cached.unwrap();
        assert_eq!(cached.0, assignment);
        drop(done);
        let ack =
            discovery::DiscoveryRequest::decode(server.requests.recv().await.unwrap()).unwrap();
        assert_eq!(ack.version_info, "accepted-version");
        assert_eq!(ack.response_nonce, "old-nonce");
        server.responses.send(Ok(None)).unwrap();

        // Observe backoff start instead of guessing with wall-clock sleeps.
        let backoff = sleeps.recv().await.unwrap();
        let gate = if matches!(phase, ReconnectPhase::Backoff) {
            None
        } else {
            Some(gates.recv().await.unwrap())
        };
        let replay_started = tokio::time::Instant::now();
        // A stays alive, so the last-watcher cache eviction cannot occur.
        let mut b = client.watch::<EdsResource>("cached-eds").await;
        let (result, done) = tokio::time::timeout(Duration::from_millis(100), next_changed(&mut b))
            .await
            .expect("cached replay blocked on reconnect");
        let replayed = result.unwrap();
        assert_eq!(replayed.0, assignment);
        assert!(Arc::ptr_eq(&cached, &replayed));
        // Keep the detached replay token across reconnection.
        assert!(servers.try_recv().is_err());
        assert!(
            gates.try_recv().is_err(),
            "commands restarted the connection attempt"
        );
        let gate = match gate {
            Some(gate) => {
                assert!(
                    !gate.is_closed(),
                    "commands cancelled the connection attempt"
                );
                gate
            }
            None => {
                assert!(replay_started.elapsed() < backoff);
                gates.recv().await.unwrap()
            }
        };

        // Register and remove another subscription while setup is pending.
        let removed = client.watch::<EdsResource>("removed-eds").await;
        drop(removed);
        let _added = client.watch::<EdsResource>("added-eds").await;
        // A cached replay is a barrier for the preceding commands.
        let mut barrier = client.watch::<EdsResource>("cached-eds").await;
        drop(next_changed(&mut barrier).await);
        gate.send(()).unwrap();
        let mut replacement = servers.recv().await.unwrap();
        let initial =
            discovery::DiscoveryRequest::decode(replacement.requests.recv().await.unwrap())
                .unwrap();
        let mut names = initial.resource_names;
        names.sort();
        assert_eq!(names, ["added-eds", "cached-eds"]);
        assert_eq!(initial.version_info, "accepted-version");
        assert!(initial.response_nonce.is_empty());
        // Neither the detached replay token nor old-session state blocks new reads.
        response.version_info = "next-version".into();
        response.nonce = "new-nonce".into();
        replacement
            .responses
            .send(Ok(Some(response.encode_to_vec().into())))
            .unwrap();
        let (updated, updated_done) = next_changed(&mut b).await;
        let updated = updated.unwrap();
        assert_eq!(updated.0, assignment);
        assert!(!Arc::ptr_eq(&cached, &updated));
        drop((done, updated_done));
    }

    #[cfg(feature = "codegen-prost")]
    #[tokio::test(start_paused = true)]
    async fn cached_eds_replayed_during_transport_build() {
        cached_eds_during_reconnect(ReconnectPhase::Build).await;
    }

    #[cfg(feature = "codegen-prost")]
    #[tokio::test(start_paused = true)]
    async fn cached_eds_replayed_during_stream_creation() {
        cached_eds_during_reconnect(ReconnectPhase::Stream).await;
    }

    #[cfg(feature = "codegen-prost")]
    #[tokio::test(start_paused = true)]
    async fn cached_eds_replayed_during_backoff() {
        cached_eds_during_reconnect(ReconnectPhase::Backoff).await;
    }

    #[tokio::test(start_paused = true)]
    async fn resource_timer_waits_for_initial_stream() {
        for phase in [ReconnectPhase::Build, ReconnectPhase::Stream] {
            let (inner, mut servers) = mock_transport();
            let (gates_tx, mut gates) = mpsc::unbounded_channel();
            let timeout = Duration::from_secs(15);
            let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
                .with_resource_initial_timeout(Some(timeout));
            let client = XdsClient::builder(
                config,
                GatedBuilder {
                    inner,
                    phase,
                    gates: gates_tx,
                },
                FakeCodec,
                TokioRuntime,
            )
            .build();
            let mut watcher = client.watch::<TestResource>("res-0").await;
            let gate = gates.recv().await.unwrap();
            tokio::time::advance(timeout * 2).await;
            assert_no_event(
                &mut watcher,
                "resource expired before the initial stream was ready",
            )
            .await;
            assert!(!gate.is_closed());
            assert!(servers.try_recv().is_err());
            gate.send(()).unwrap();
            let mut server = servers.recv().await.unwrap();
            server.requests.recv().await.unwrap();
            tokio::time::advance(timeout - Duration::from_secs(1)).await;
            assert_no_event(&mut watcher, "setup time shortened the resource deadline").await;
            let (result, _done) =
                tokio::time::timeout(Duration::from_secs(1), next_changed(&mut watcher))
                    .await
                    .expect("initial subscription did not start a timer");
            assert!(matches!(result, Err(Error::ResourceDoesNotExist)));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn additional_watcher_does_not_reset_resource_timer() {
        let (builder, mut servers) = mock_transport();
        let (sleeps_tx, mut sleeps) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout));
        let client = XdsClient::builder(
            config,
            builder,
            FakeCodec,
            ObservedRuntime { sleeps: sleeps_tx },
        )
        .build();
        let mut first = client.watch::<TestResource>("res-0").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        assert_eq!(sleeps.recv().await.unwrap(), timeout);
        tokio::time::advance(Duration::from_secs(10)).await;
        let mut second = client.watch::<TestResource>("res-0").await;
        // Changing another subscription exercises the shared send/timer path too.
        let _other = client.watch::<TestResource>("res-1").await;
        server.requests.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        for watcher in [&mut first, &mut second] {
            let (result, _done) =
                tokio::time::timeout(Duration::from_millis(100), next_changed(watcher))
                    .await
                    .expect("additional watcher reset the original deadline");
            assert!(matches!(result, Err(Error::ResourceDoesNotExist)));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn named_watch_under_wildcard_starts_resource_timer() {
        let (builder, mut servers) = mock_transport();
        let (sleeps_tx, mut sleeps) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout));
        let client = XdsClient::builder(
            config,
            builder,
            FakeCodec,
            ObservedRuntime { sleeps: sleeps_tx },
        )
        .build();
        let _wildcard = client.watch::<TestResource>("").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        let mut named = client.watch::<TestResource>("res-0").await;
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), sleeps.recv())
                .await
                .expect("named watch under wildcard did not start a timer")
                .unwrap(),
            timeout,
        );
        assert!(
            server.requests.try_recv().is_err(),
            "wildcard already covers the named resource"
        );
        tokio::time::advance(timeout).await;
        let (result, _done) = next_changed(&mut named).await;
        assert!(matches!(result, Err(Error::ResourceDoesNotExist)));
    }

    #[tokio::test(start_paused = true)]
    async fn cached_resource_does_not_start_another_timer() {
        let (builder, mut servers) = mock_transport();
        let (sleeps_tx, mut sleeps) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout));
        let client = XdsClient::builder(
            config,
            builder,
            FakeCodec,
            ObservedRuntime { sleeps: sleeps_tx },
        )
        .build();
        let mut first = client.watch::<TestResource>("res-0").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        assert_eq!(sleeps.recv().await.unwrap(), timeout);
        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let (original, done) = next_changed(&mut first).await;
        drop(done);
        server.requests.recv().await.unwrap();
        let mut cached = client.watch::<TestResource>("res-0").await;
        let (replayed, done) = next_changed(&mut cached).await;
        assert!(Arc::ptr_eq(&original.unwrap(), &replayed.unwrap()));
        drop(done);
        // A new unresolved name should be the only new timer on this request.
        let _other = client.watch::<TestResource>("res-1").await;
        server.requests.recv().await.unwrap();
        assert_eq!(sleeps.recv().await.unwrap(), timeout);
        tokio::time::advance(timeout * 2).await;
        assert_no_event(&mut first, "cached resource expired").await;
        assert_no_event(&mut cached, "cached replay started a timer").await;
        assert!(
            sleeps.try_recv().is_err(),
            "cached resource started an extra timer"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resource_timer_is_stopped_during_backoff() {
        let (builder, mut servers) = mock_transport();
        let (sleeps_tx, mut sleeps) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let backoff = Duration::from_secs(60);
        let policy = crate::RetryPolicy::default()
            .with_max_backoff(backoff)
            .unwrap()
            .with_initial_backoff(backoff)
            .unwrap()
            .with_jitter(0.0)
            .unwrap();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout))
            .with_retry_policy(policy);
        let client = XdsClient::builder(
            config,
            builder,
            FakeCodec,
            ObservedRuntime { sleeps: sleeps_tx },
        )
        .build();
        let mut first = client.watch::<TestResource>("res-0").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        assert_eq!(sleeps.recv().await.unwrap(), timeout);
        server.responses.send(Ok(None)).unwrap();
        assert_eq!(sleeps.recv().await.unwrap(), backoff);
        let mut second = client.watch::<TestResource>("res-1").await;
        tokio::time::advance(timeout * 2).await;
        assert_no_event(&mut first, "existing resource expired during backoff").await;
        assert_no_event(&mut second, "new resource expired during backoff").await;
        assert!(servers.try_recv().is_err(), "backoff ended early");
        assert!(
            sleeps.try_recv().is_err(),
            "new watch started a timer during backoff"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn uncached_resource_does_not_expire_while_reconnect_is_pending() {
        for phase in [ReconnectPhase::Build, ReconnectPhase::Stream] {
            uncached_resource_during_reconnect(phase).await;
        }
    }

    async fn uncached_resource_during_reconnect(phase: ReconnectPhase) {
        let (inner, mut servers) = mock_transport();
        let (gates_tx, mut gates) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout));
        let client = XdsClient::builder(
            config,
            GatedBuilder {
                inner,
                phase,
                gates: gates_tx,
            },
            FakeCodec,
            TokioRuntime,
        )
        .build();
        let mut cached = client.watch::<TestResource>("res-0").await;
        gates.recv().await.unwrap().send(()).unwrap();
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let (resource, done) = next_changed(&mut cached).await;
        assert!(resource.is_ok());
        drop(done);
        server.requests.recv().await.unwrap();
        server.responses.send(Ok(None)).unwrap();
        let gate = gates.recv().await.unwrap();

        let mut unresolved = client.watch::<TestResource>("res-1").await;
        // Cached replay confirms the preceding registration was processed.
        let mut barrier = client.watch::<TestResource>("res-0").await;
        assert!(next_changed(&mut barrier).await.0.is_ok());

        tokio::time::advance(timeout).await;
        let event = tokio::time::timeout(Duration::from_millis(100), unresolved.next()).await;
        assert!(!gate.is_closed(), "reconnect attempt was cancelled");
        assert!(servers.try_recv().is_err(), "reconnect gate was bypassed");
        assert!(
            event.is_err(),
            "uncached resource expired before its subscription was sent: {event:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resource_timer_is_cancelled_on_disconnect_and_restarted_on_reconnect() {
        let (inner, mut servers) = mock_transport();
        let (gates_tx, mut gates) = mpsc::unbounded_channel();
        let timeout = Duration::from_secs(15);
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(timeout));
        let client = XdsClient::builder(
            config,
            GatedBuilder {
                inner,
                phase: ReconnectPhase::Build,
                gates: gates_tx,
            },
            FakeCodec,
            TokioRuntime,
        )
        .build();
        let mut watcher = client.watch::<TestResource>("res-0").await;
        gates.recv().await.unwrap().send(()).unwrap();
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        // Part of the first deadline elapses without a resource response.
        tokio::time::advance(Duration::from_secs(5)).await;
        server.responses.send(Ok(None)).unwrap();
        let gate = gates.recv().await.unwrap();

        // Disconnected time must not cause a resource absence notification.
        tokio::time::advance(timeout).await;
        assert!(!gate.is_closed(), "reconnect attempt was cancelled");
        assert!(servers.try_recv().is_err(), "reconnect gate was bypassed");
        assert_no_event(&mut watcher, "resource timer kept running after disconnect").await;

        gate.send(()).unwrap();
        let mut replacement = servers.recv().await.unwrap();
        replacement.requests.recv().await.unwrap();
        // An expiration queued by the first stream must not consume the
        // replacement stream's timer for the same resource.
        client
            .command_tx
            .send(WorkerCommand::Watcher(WatchEvent::ResourceTimerExpired {
                type_url: TEST_TYPE_URL.to_string(),
                name: "res-0".to_string(),
                timer_id: 0,
            }))
            .unwrap();
        assert_no_event(
            &mut watcher,
            "stale expiration consumed the replacement timer",
        )
        .await;
        // Reconnection grants a full deadline, rather than the remaining time
        // from the first stream. No server response is needed for expiration.
        tokio::time::advance(timeout - Duration::from_secs(1)).await;
        assert_no_event(&mut watcher, "reconnect did not restart the full deadline").await;
        let (result, _done) =
            tokio::time::timeout(Duration::from_secs(1), next_changed(&mut watcher))
                .await
                .expect("resource timer was not restarted after reconnect");
        assert!(matches!(result, Err(Error::ResourceDoesNotExist)));
    }

    #[tokio::test(start_paused = true)]
    async fn final_watcher_drop_cancels_pending_connection() {
        for phase in [ReconnectPhase::Build, ReconnectPhase::Stream] {
            let (inner, _servers) = mock_transport();
            let (gates_tx, mut gates) = mpsc::unbounded_channel();
            let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
                .with_resource_initial_timeout(None);
            let client = XdsClient::builder(
                config,
                GatedBuilder {
                    inner,
                    phase,
                    gates: gates_tx,
                },
                FakeCodec,
                TokioRuntime,
            )
            .build();
            let watcher = client.watch::<TestResource>("res-0").await;
            let mut gate = gates.recv().await.unwrap();
            drop(client);
            // A watcher alone keeps the actor and its pending connection alive.
            assert!(
                tokio::time::timeout(Duration::from_millis(100), gate.closed())
                    .await
                    .is_err()
            );
            drop(watcher);
            tokio::time::timeout(Duration::from_millis(100), gate.closed())
                .await
                .expect("pending connection outlived final watcher");
            assert!(
                gates.recv().await.is_none(),
                "transport builder was retained"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn old_session_deliveries_and_tokens_survive_reconnect() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(None);
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();
        let mut watcher = client.watch::<TestResource>("res-0").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        assert_eq!(
            parse_request(&server.requests.recv().await.unwrap()),
            ("1".into(), "n1".into())
        );

        // Keep the committed event queued. Fail a write while the reader waits
        // for its completion token, forcing disposal of that entire session.
        drop(server.requests);
        let _extra = client.watch::<TestResource>("res-1").await;
        let mut replacement = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("held token blocked reconnect")
            .unwrap();
        assert_eq!(
            parse_request(&replacement.requests.recv().await.unwrap()),
            ("1".into(), "".into())
        );
        let (old, old_done) = next_changed(&mut watcher).await;
        assert!(old.is_ok(), "committed delivery was lost on disconnect");
        replacement
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0"]))))
            .unwrap();
        let (updated, new_done) = next_changed(&mut watcher).await;
        assert!(updated.is_ok(), "old token blocked replacement stream");

        replacement
            .responses
            .send(Ok(Some(response("3", "n3", &["res-0"]))))
            .unwrap();
        drop(old_done);
        assert_no_event(&mut watcher, "old token resumed replacement stream").await;
        drop(new_done);
        assert!(next_changed(&mut watcher).await.0.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn undecodable_response_retires_session_without_committing_later_data() {
        let (builder, mut servers) = mock_transport();
        let recorder = Arc::new(CapturingRecorder::default());
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(None);
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime)
            .with_metrics_recorder(recorder.clone())
            .build();
        let mut watcher = client.watch::<TestResource>("res-0").await;
        let mut old = servers.recv().await.unwrap();
        old.requests.recv().await.unwrap();
        old.responses
            .send(Ok(Some(Bytes::from_static(&[0xff]))))
            .unwrap();
        old.responses
            .send(Ok(Some(response(
                "stale-version",
                "stale-nonce",
                &["res-0"],
            ))))
            .unwrap();

        let mut replacement = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("undecodable response did not close the stream")
            .unwrap();
        assert_eq!(
            parse_request(&replacement.requests.recv().await.unwrap()),
            ("".into(), "".into())
        );
        assert_no_event(&mut watcher, "retired session committed stale data").await;
        let events = recorder.take();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.instrument == "grpc.xds_client.server_failure")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.instrument == "grpc.xds_client.connected"
                    && e.kind == Measurement::Gauge(1))
                .count(),
            1
        );

        replacement
            .responses
            .send(Ok(Some(response("1", "new-nonce", &["res-0"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());
        assert!(recorder.take().iter().any(
            |e| e.instrument == "grpc.xds_client.connected" && e.kind == Measurement::Gauge(1)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_starts_on_first_watch_and_keeps_reconnecting() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(None);
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), servers.recv())
                .await
                .is_err(),
            "connected without a subscription"
        );
        let watcher = client.watch::<TestResource>("res-0").await;
        let mut server = servers.recv().await.unwrap();
        server.requests.recv().await.unwrap();
        drop(watcher);
        server.responses.send(Ok(None)).unwrap();
        let mut replacement = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("lifecycle stopped after the last unwatch")
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), replacement.requests.recv())
                .await
                .is_err(),
            "sent a request without subscriptions"
        );
        let _watcher = client.watch::<TestResource>("res-2").await;
        let request = replacement.requests.recv().await.unwrap();
        assert!(
            String::from_utf8(request.to_vec())
                .unwrap()
                .ends_with("res-2")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transport_resets_retry_limit_only_after_a_decoded_response() {
        for valid_response in [false, true] {
            let (builder, mut servers) = mock_transport();
            let policy = crate::RetryPolicy::default()
                .with_initial_backoff(Duration::from_millis(100))
                .unwrap()
                .with_jitter(0.0)
                .unwrap()
                .with_max_attempts(Some(1));
            let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
                .with_resource_initial_timeout(None)
                .with_retry_policy(policy);
            let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();
            let mut watcher = client.watch::<TestResource>("res-0").await;
            let mut first = servers.recv().await.unwrap();
            first.requests.recv().await.unwrap();
            first.responses.send(Ok(None)).unwrap();
            let mut second = servers.recv().await.unwrap();
            second.requests.recv().await.unwrap();
            if valid_response {
                second
                    .responses
                    .send(Ok(Some(response("1", "n1", &["res-0"]))))
                    .unwrap();
                let (resource, done) = next_changed(&mut watcher).await;
                assert!(resource.is_ok());
                drop(done);
                second.requests.recv().await.unwrap();
                second.responses.send(Ok(None)).unwrap();
                let mut third = tokio::time::timeout(Duration::from_secs(1), servers.recv())
                    .await
                    .expect("decoded response did not reset retry limit")
                    .unwrap();
                third.requests.recv().await.unwrap();
            } else {
                second
                    .responses
                    .send(Ok(Some(Bytes::from_static(&[0xff]))))
                    .unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), servers.recv())
                        .await
                        .expect("retry limit did not stop the transport")
                        .is_none(),
                    "undecodable response reset retry limit"
                );
            }
        }
    }

    /// Watch `name` and wait for the resulting subscription request, so the
    /// watcher is registered before the test sends a response.
    async fn watch_synced(
        client: &XdsClient,
        server: &mut MockServer,
        name: &str,
    ) -> ResourceWatcher<TestResource> {
        let watcher = client.watch::<TestResource>(name).await;
        let _request = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("timed out waiting for subscription request")
            .expect("stream closed");
        watcher
    }

    /// Next event, unwrapped to its result and `ProcessingDone` token.
    async fn next_changed<T: Resource>(
        watcher: &mut ResourceWatcher<T>,
    ) -> (
        Result<std::sync::Arc<T>>,
        crate::client::watch::ProcessingDone,
    ) {
        let event = tokio::time::timeout(Duration::from_secs(5), watcher.next())
            .await
            .expect("timed out waiting for event")
            .expect("watcher closed");
        match event {
            ResourceEvent::ResourceChanged { result, done } => (result, done),
            ResourceEvent::AmbientError { .. } => panic!("unexpected ambient error"),
        }
    }

    /// Asserts no event is delivered to `watcher` within a short window.
    async fn assert_no_event<T: Resource>(watcher: &mut ResourceWatcher<T>, message: &str) {
        assert!(
            tokio::time::timeout(Duration::from_millis(200), watcher.next())
                .await
                .is_err(),
            "{message}"
        );
    }

    #[tokio::test]
    async fn connected_metric_lifecycle() {
        let recorder = Arc::new(CapturingRecorder::default());
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime)
            .with_metrics_recorder(recorder.clone())
            .build();

        // Initial state: connected = 1 is recorded for the server URI
        let mut watcher = client.watch::<TestResource>("res-0").await;
        let mut server1 = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream 1")
            .expect("transport dropped");
        let _ = tokio::time::timeout(Duration::from_secs(5), server1.requests.recv())
            .await
            .expect("timed out waiting for initial request")
            .expect("stream closed");

        let events = recorder.take();
        let connected = events
            .iter()
            .find(|e| e.instrument == "grpc.xds_client.connected");
        assert_eq!(connected.map(|e| &e.kind), Some(&Measurement::Gauge(1)));
        assert_eq!(
            attr(connected.unwrap(), "grpc.xds.server"),
            Some("mock:///xds")
        );

        // Stream closes without seeing a response: transitions to connected = 0 and increments
        // server_failure
        server1.responses.send(Ok(None)).unwrap();

        // Wait for worker to reconnect with stream 2
        let mut server2 = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream 2")
            .expect("transport dropped");
        let _ = tokio::time::timeout(Duration::from_secs(5), server2.requests.recv())
            .await
            .expect("timed out waiting for request on stream 2")
            .expect("stream closed");

        let events = recorder.take();
        assert_eq!(
            events
                .iter()
                .find(|e| e.instrument == "grpc.xds_client.connected")
                .map(|e| &e.kind),
            Some(&Measurement::Gauge(0))
        );
        assert_eq!(
            events
                .iter()
                .find(|e| e.instrument == "grpc.xds_client.server_failure")
                .map(|e| &e.kind),
            Some(&Measurement::CounterU64(1))
        );

        // First response arrives on reconnected stream: resets connected = 1
        server2
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let (result, done) = next_changed(&mut watcher).await;
        assert!(result.is_ok());
        drop(done);

        let events = recorder.take();
        assert_eq!(
            events
                .iter()
                .find(|e| e.instrument == "grpc.xds_client.connected")
                .map(|e| &e.kind),
            Some(&Measurement::Gauge(1))
        );

        // Stream closes after seeing a response: does NOT count as server failure
        server2.responses.send(Ok(None)).unwrap();

        let mut server3 = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream 3")
            .expect("transport dropped");
        let _ = tokio::time::timeout(Duration::from_secs(5), server3.requests.recv())
            .await
            .expect("timed out waiting for request on stream 3")
            .expect("stream closed");

        let events = recorder.take();
        assert!(!events.iter().any(|e| {
            e.instrument == "grpc.xds_client.connected"
                || e.instrument == "grpc.xds_client.server_failure"
        }));

        // Worker shuts down when client is dropped: connected resets to 0
        drop(client);
        drop(watcher);

        assert!(
            tokio::time::timeout(Duration::from_secs(5), servers.recv())
                .await
                .expect("timed out waiting for worker shutdown")
                .is_none()
        );

        let events = recorder.take();
        assert!(events.iter().any(
            |e| e.instrument == "grpc.xds_client.connected" && e.kind == Measurement::Gauge(0)
        ));
    }

    struct FailingTransportBuilder {
        attempt_tx: Mutex<Option<oneshot::Sender<()>>>,
    }

    impl TransportBuilder for FailingTransportBuilder {
        type Transport = MockTransport;

        async fn build(&self, _server: &ServerConfig) -> Result<Self::Transport> {
            let _ = self.attempt_tx.lock().unwrap().take();
            Err(Error::StreamClosed)
        }
    }

    #[tokio::test]
    async fn initial_connection_failure_records_server_failure() {
        let (attempt_tx, attempt_rx) = oneshot::channel();
        let builder = FailingTransportBuilder {
            attempt_tx: Mutex::new(Some(attempt_tx)),
        };
        let recorder = Arc::new(CapturingRecorder::default());

        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime)
            .with_metrics_recorder(recorder.clone())
            .build();

        let _watcher = client.watch::<TestResource>("res-0").await;

        tokio::time::timeout(Duration::from_secs(5), attempt_rx)
            .await
            .expect("timed out waiting for initial connection failure")
            .unwrap_err();

        let events = recorder.take();
        assert!(
            events
                .iter()
                .any(|e| e.instrument == "grpc.xds_client.server_failure")
        );
        assert!(events.iter().any(
            |e| e.instrument == "grpc.xds_client.connected" && e.kind == Measurement::Gauge(0)
        ));
    }

    /// A watcher that issues more commands than the command channel buffers
    /// while holding its `ProcessingDone` token must not deadlock the worker.
    ///
    /// Before the flow-control fix the worker awaited the token inside
    /// `handle_response`, so it stopped draining commands; once the channel
    /// filled, watcher and worker waited on each other forever.
    #[tokio::test]
    async fn commands_drain_while_processing_done_is_held() {
        let (client, mut watcher, server) = connected_client().await;

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), watcher.next())
            .await
            .expect("timed out waiting for event")
            .expect("watcher closed");
        let ResourceEvent::ResourceChanged {
            result: Ok(_),
            done,
        } = event
        else {
            panic!("expected ResourceChanged(Ok)");
        };

        // More commands than the previous bounded queue could hold.
        let mut extra = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            for i in 1..=100 {
                extra.push(client.watch::<TestResource>(format!("res-{i}")).await);
            }
        })
        .await
        .expect("deadlock: commands not drained while ProcessingDone was held");

        // The worker is still healthy end-to-end: it reads the next response
        // and delivers it.
        drop(done);
        server
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0"]))))
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), watcher.next())
            .await
            .expect("timed out waiting for second event")
            .expect("watcher closed");
        assert!(matches!(
            event,
            ResourceEvent::ResourceChanged { result: Ok(_), .. }
        ));
    }

    /// The ACK goes out as soon as the response is validated and cached, and
    /// the *next* response is not delivered until the previous one's
    /// `ProcessingDone` tokens drop (ADS flow control).
    #[tokio::test]
    async fn next_response_gated_until_processing_done() {
        let (_client, mut watcher, mut server) = connected_client().await;

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), watcher.next())
            .await
            .expect("timed out waiting for event")
            .expect("watcher closed");
        let ResourceEvent::ResourceChanged {
            result: Ok(_),
            done,
        } = event
        else {
            panic!("expected ResourceChanged(Ok)");
        };

        // ACK is not gated on ProcessingDone.
        let ack = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("ACK not sent while ProcessingDone was held")
            .expect("stream closed");
        assert_eq!(parse_request(&ack), ("1".to_string(), "n1".to_string()));

        // The next response is gated on ProcessingDone.
        server
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0"]))))
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), watcher.next())
                .await
                .is_err(),
            "second response delivered while the first was still being processed"
        );

        drop(done);
        let event = tokio::time::timeout(Duration::from_secs(5), watcher.next())
            .await
            .expect("timed out waiting for second event")
            .expect("watcher closed");
        assert!(matches!(
            event,
            ResourceEvent::ResourceChanged { result: Ok(_), .. }
        ));
        let ack = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("second ACK not sent")
            .expect("stream closed");
        assert_eq!(parse_request(&ack), ("2".to_string(), "n2".to_string()));
    }

    /// The next response is gated until *every* watcher drops its
    /// `ProcessingDone` token, not just the first one.
    #[tokio::test]
    async fn next_response_gated_until_all_watchers_signal() {
        let (client, mut w1, mut server) = connected_client().await;
        let mut w2 = watch_synced(&client, &mut server, "res-1").await;

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0", "res-1"]))))
            .unwrap();
        let (r1, done1) = next_changed(&mut w1).await;
        let (r2, done2) = next_changed(&mut w2).await;
        assert!(r1.is_ok() && r2.is_ok());

        drop(done1);
        server
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0", "res-1"]))))
            .unwrap();
        assert_no_event(&mut w1, "second response delivered while a token was held").await;

        drop(done2);
        assert!(next_changed(&mut w1).await.0.is_ok());
        assert!(next_changed(&mut w2).await.0.is_ok());
    }

    /// Validation-error events gate flow control like regular updates:
    /// the response is NACKed, valid resources are still delivered, and
    /// holding the error token delays the next response.
    #[tokio::test]
    async fn error_events_gate_next_response() {
        let (client, mut w_ok, mut server) = connected_client().await;
        let mut w_bad = watch_synced(&client, &mut server, "bad-0").await;

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0", "bad-0"]))))
            .unwrap();
        let (result, done_ok) = next_changed(&mut w_ok).await;
        assert!(result.is_ok());
        let (result, err_done) = next_changed(&mut w_bad).await;
        assert!(matches!(result, Err(Error::Validation(_))));

        // NACK keeps the old (empty) version.
        let nack = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("NACK not sent")
            .expect("stream closed");
        assert_eq!(parse_request(&nack), ("".to_string(), "n1".to_string()));

        drop(done_ok);
        server
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0"]))))
            .unwrap();
        assert_no_event(
            &mut w_ok,
            "response delivered while the error token was held",
        )
        .await;

        drop(err_done);
        assert!(next_changed(&mut w_ok).await.0.is_ok());
    }

    /// Deletion events (SotW resource missing from a response, gRFC A53)
    /// gate the next response like regular updates.
    #[tokio::test]
    async fn deletion_events_gate_next_response() {
        let (_client, mut watcher, server) = connected_client_for::<SotwResource>().await;

        server
            .responses
            .send(Ok(Some(sotw_response("1", "n1", &["res-0"]))))
            .unwrap();
        let (result, done) = next_changed(&mut watcher).await;
        assert!(result.is_ok());
        drop(done);

        // res-0 missing from the SotW response: deleted.
        server
            .responses
            .send(Ok(Some(sotw_response("2", "n2", &[]))))
            .unwrap();
        let (result, deletion_done) = next_changed(&mut watcher).await;
        assert!(matches!(result, Err(Error::ResourceDoesNotExist)));

        server
            .responses
            .send(Ok(Some(sotw_response("3", "n3", &["res-0"]))))
            .unwrap();
        assert_no_event(
            &mut watcher,
            "response delivered while the deletion token was held",
        )
        .await;

        drop(deletion_done);
        assert!(next_changed(&mut watcher).await.0.is_ok());
    }

    /// Events staged for a watcher that was dropped must not wedge flow
    /// control: their failed sends release the shared signal.
    #[tokio::test]
    async fn dropped_watcher_does_not_stall_flow_control() {
        let (client, mut w1, mut server) = connected_client().await;
        drop(watch_synced(&client, &mut server, "res-1").await);

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0", "res-1"]))))
            .unwrap();
        let (result, done) = next_changed(&mut w1).await;
        assert!(result.is_ok());
        drop(done);

        server
            .responses
            .send(Ok(Some(response("2", "n2", &["res-0", "res-1"]))))
            .unwrap();
        assert!(next_changed(&mut w1).await.0.is_ok());
    }

    /// A wildcard watcher receiving a response with more resources than
    /// the previous watcher buffer size (16) must not deadlock the worker even
    /// before the watcher reads any events, and ADS flow control must remain
    /// gated until all delivered tokens are dropped.
    #[tokio::test]
    async fn wildcard_watcher_many_resources_does_not_deadlock() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();

        let mut watcher = client.watch::<TestResource>("").await;
        let mut server = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream")
            .expect("transport dropped");
        let _initial = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("timed out waiting for initial request")
            .expect("stream closed");

        let names: Vec<String> = (0..50).map(|i| format!("res-{i}")).collect();
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        server
            .responses
            .send(Ok(Some(response("1", "n1", &name_refs))))
            .unwrap();

        // Wait for ACK to confirm worker processed the response.
        let ack = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("ACK not sent")
            .expect("stream closed");
        assert_eq!(parse_request(&ack), ("1".to_string(), "n1".to_string()));

        // Worker is not deadlocked even though 50 events were produced and none
        // have been read from `watcher` yet: issuing a command succeeds.
        let mut extra = tokio::time::timeout(
            Duration::from_secs(5),
            client.watch::<TestResource>("extra"),
        )
        .await
        .expect("worker deadlocked on full wildcard channel");

        // The wildcard watcher has consumed nothing. A second watcher must
        // still reach the actor and receive the cached resource.
        let mut cached = client.watch::<TestResource>("res-0").await;
        assert!(next_changed(&mut cached).await.0.is_ok());

        let mut dones = Vec::new();
        for _ in 0..50 {
            let (res, done) = next_changed(&mut watcher).await;
            assert!(res.is_ok());
            dones.push(done);
        }

        // Send a second response for `extra`. It should be gated until all 50 tokens drop.
        server
            .responses
            .send(Ok(Some(response("2", "n2", &["extra"]))))
            .unwrap();
        assert_no_event(
            &mut extra,
            "second response delivered before all tokens dropped",
        )
        .await;

        drop(dones);
        assert!(next_changed(&mut extra).await.0.is_ok());
    }

    /// When the stream closes, the worker reconnects with a new stream and
    /// re-subscribes active watchers.
    #[tokio::test]
    async fn stream_closed_triggers_reconnect() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();

        let mut watcher = client.watch::<TestResource>("res-0").await;
        let mut server1 = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for stream")
            .expect("transport dropped");
        let _initial1 = tokio::time::timeout(Duration::from_secs(5), server1.requests.recv())
            .await
            .expect("timed out waiting for initial request")
            .expect("stream closed");

        // Close the first stream.
        server1.responses.send(Ok(None)).unwrap();

        // Worker should reconnect on a new stream.
        let mut server2 = tokio::time::timeout(Duration::from_secs(5), servers.recv())
            .await
            .expect("timed out waiting for reconnected stream")
            .expect("transport dropped");

        // Reconnected stream should send initial requests for active watchers.
        let initial2 = tokio::time::timeout(Duration::from_secs(5), server2.requests.recv())
            .await
            .expect("timed out waiting for initial request on new stream")
            .expect("stream closed");
        let (version, _nonce) = parse_request(&initial2);
        assert_eq!(version, "");

        // Sending a response on the new stream delivers to the watcher.
        server2
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let (result, _done) = next_changed(&mut watcher).await;
        assert!(result.is_ok());
    }

    /// Multiple watcher commands can be enqueued and sent over the unbounded
    /// write channel even while flow control is gating the next response.
    #[tokio::test]
    async fn unbounded_writes_sent_while_flow_control_is_held() {
        let (client, mut w0, mut server) = connected_client().await;

        server
            .responses
            .send(Ok(Some(response("1", "n1", &["res-0"]))))
            .unwrap();
        let (_result, done) = next_changed(&mut w0).await;

        // Drain ACK for res-0.
        let ack = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("ACK not sent")
            .expect("stream closed");
        assert_eq!(parse_request(&ack), ("1".to_string(), "n1".to_string()));

        // While `done` is held, add two more watches.
        let _w1 = client.watch::<TestResource>("res-1").await;
        let _w2 = client.watch::<TestResource>("res-2").await;

        // Both requests should arrive on the stream in order.
        let req1 = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("req1 timed out")
            .expect("stream closed");
        let text1 = String::from_utf8(req1.to_vec()).unwrap();
        assert!(text1.contains("res-1"));

        let req2 = tokio::time::timeout(Duration::from_secs(5), server.requests.recv())
            .await
            .expect("req2 timed out")
            .expect("stream closed");
        let text2 = String::from_utf8(req2.to_vec()).unwrap();
        assert!(text2.contains("res-2"));

        drop(done);
    }

    async fn take_snapshot(client: &XdsClient) -> Vec<ResourceSnapshot> {
        tokio::time::timeout(Duration::from_secs(5), client.resource_snapshot())
            .await
            .expect("timed out waiting for snapshot")
    }

    fn versioned_response(version: &str, nonce: &str, names: &[&str]) -> Bytes {
        response_for(VERSIONED_TYPE_URL, version, nonce, names)
    }

    /// A distinct nonce for each response. These tests only check versions.
    fn next_nonce() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed).to_string()
    }

    #[tokio::test]
    async fn snapshot_reports_requested_resource() {
        let (client, _watcher, _server) = connected_client().await;

        let snapshot = take_snapshot(&client).await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].type_url, TEST_TYPE_URL);
        assert_eq!(snapshot[0].name, "res-0");
        assert_eq!(snapshot[0].status, ResourceStatus::Requested);
        assert!(snapshot[0].accepted.is_none());
        assert!(snapshot[0].rejected.is_none());
    }

    #[tokio::test]
    async fn snapshot_reports_accepted_resource() {
        let (client, mut watcher, server) = connected_client().await;
        let before = SystemTime::now();
        server
            .responses
            .send(Ok(Some(response("1", &next_nonce(), &["res-0"]))))
            .unwrap();
        let (result, done) = next_changed(&mut watcher).await;
        assert!(result.is_ok());

        // Answered while the response's flow-control token is still held.
        let snapshot = take_snapshot(&client).await;
        drop(done);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].status, ResourceStatus::Acked);
        let accepted = snapshot[0].accepted.as_ref().unwrap();
        assert_eq!(accepted.version_info, "1");
        assert_eq!(accepted.resource.type_url, TEST_TYPE_URL);
        assert_eq!(accepted.resource.value, Bytes::from("res-0"));
        assert!(accepted.last_updated >= before);
        assert!(snapshot[0].rejected.is_none());
    }

    #[tokio::test]
    async fn snapshot_keeps_accepted_version_of_rejected_resource() {
        let (client, mut watcher, mut server) = connected_client_for::<VersionedResource>().await;
        server
            .responses
            .send(Ok(Some(versioned_response("1", &next_nonce(), &["res-0"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());
        let _ack = server.requests.recv().await.unwrap();

        let nack_nonce = next_nonce();
        server
            .responses
            .send(Ok(Some(versioned_response(
                "2",
                &nack_nonce,
                &["res-0#bad"],
            ))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_err());
        let nack = server.requests.recv().await.unwrap();
        assert_eq!(parse_request(&nack), ("1".to_string(), nack_nonce));

        let snapshot = take_snapshot(&client).await;
        assert_eq!(snapshot[0].status, ResourceStatus::Nacked);
        let accepted = snapshot[0].accepted.as_ref().unwrap();
        assert_eq!(accepted.version_info, "1");
        assert_eq!(accepted.resource.value, Bytes::from("res-0"));
        let rejected = snapshot[0].rejected.as_ref().unwrap();
        assert_eq!(rejected.version_info, "2");
        assert_eq!(rejected.details, "resource validation failed: res-0 is bad");
        assert!(rejected.last_update_attempt >= accepted.last_updated);

        // A later accepted version clears the rejection.
        server
            .responses
            .send(Ok(Some(versioned_response("3", &next_nonce(), &["res-0"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());
        let snapshot = take_snapshot(&client).await;
        assert_eq!(snapshot[0].status, ResourceStatus::Acked);
        assert_eq!(snapshot[0].accepted.as_ref().unwrap().version_info, "3");
        assert!(snapshot[0].rejected.is_none());
    }

    #[tokio::test]
    async fn snapshot_of_partially_rejected_response() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds");
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();
        let mut watchers = Vec::new();
        for name in ["res-0", "res-1", "res-2"] {
            watchers.push(client.watch::<VersionedResource>(name).await);
        }
        let server = servers.recv().await.unwrap();

        server
            .responses
            .send(Ok(Some(versioned_response(
                "1",
                &next_nonce(),
                &["res-0", "res-1#bad", "res-2#bad"],
            ))))
            .unwrap();
        for watcher in &mut watchers {
            let _ = next_changed(watcher).await;
        }

        let snapshot = take_snapshot(&client).await;
        let names: Vec<&str> = snapshot.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["res-0", "res-1", "res-2"]);

        // A valid resource is accepted even though the response was NACKed.
        assert_eq!(snapshot[0].status, ResourceStatus::Acked);
        assert_eq!(snapshot[0].accepted.as_ref().unwrap().version_info, "1");

        // Each invalid resource reports only its own error, not the whole
        // NACK error.
        for rejected in &snapshot[1..] {
            assert_eq!(rejected.status, ResourceStatus::Nacked);
            assert!(rejected.accepted.is_none());
            let update = rejected.rejected.as_ref().unwrap();
            assert_eq!(update.version_info, "1");
            assert_eq!(
                update.details,
                format!("resource validation failed: {} is bad", rejected.name)
            );
        }
    }

    #[tokio::test]
    async fn snapshot_keeps_accepted_version_of_deleted_resource() {
        let (client, mut watcher, server) = connected_client_for::<SotwResource>().await;
        server
            .responses
            .send(Ok(Some(sotw_response("1", &next_nonce(), &["res-0"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());

        server
            .responses
            .send(Ok(Some(sotw_response("2", &next_nonce(), &[]))))
            .unwrap();
        let (result, _done) = next_changed(&mut watcher).await;
        assert!(matches!(result, Err(Error::ResourceDoesNotExist)));

        let snapshot = take_snapshot(&client).await;
        assert_eq!(snapshot[0].status, ResourceStatus::DoesNotExist);
        assert_eq!(snapshot[0].accepted.as_ref().unwrap().version_info, "1");
        assert!(snapshot[0].rejected.is_none());
    }

    #[tokio::test]
    async fn snapshot_reports_timed_out_resource_as_does_not_exist() {
        let (builder, mut servers) = mock_transport();
        let config = ClientConfig::new(Node::new("test", "0"), "mock:///xds")
            .with_resource_initial_timeout(Some(Duration::from_millis(10)));
        let client = XdsClient::builder(config, builder, FakeCodec, TokioRuntime).build();
        let mut watcher = client.watch::<TestResource>("res-0").await;
        let _server = servers.recv().await.unwrap();

        let (result, _done) = next_changed(&mut watcher).await;
        assert!(matches!(result, Err(Error::ResourceDoesNotExist)));

        let snapshot = take_snapshot(&client).await;
        assert_eq!(snapshot[0].status, ResourceStatus::DoesNotExist);
        assert!(snapshot[0].accepted.is_none());
    }

    #[tokio::test]
    async fn snapshot_omits_unsubscribed_resources() {
        let (client, mut watcher, mut server) = connected_client().await;
        let other = watch_synced(&client, &mut server, "res-1").await;
        server
            .responses
            .send(Ok(Some(response("1", &next_nonce(), &["res-0", "res-1"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());
        let _ack = server.requests.recv().await.unwrap();

        // The cache keeps res-1 after its last watcher goes away, but the
        // client no longer subscribes to it.
        drop(other);
        let _unsubscribe = server.requests.recv().await.unwrap();

        let names: Vec<String> = take_snapshot(&client)
            .await
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["res-0"]);
    }

    #[tokio::test]
    async fn snapshot_includes_resources_of_wildcard_subscription() {
        let (client, mut watcher, mut server) = connected_client().await;
        let mut wildcard = watch_synced(&client, &mut server, "").await;
        server
            .responses
            .send(Ok(Some(response("1", &next_nonce(), &["res-0", "res-9"]))))
            .unwrap();
        assert!(next_changed(&mut watcher).await.0.is_ok());
        assert!(next_changed(&mut wildcard).await.0.is_ok());
        assert!(next_changed(&mut wildcard).await.0.is_ok());

        let names: Vec<String> = take_snapshot(&client)
            .await
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["res-0", "res-9"]);
    }

    #[tokio::test]
    async fn snapshot_of_stopped_worker_is_empty() {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        drop(command_rx);
        let client = XdsClient { command_tx };
        assert!(take_snapshot(&client).await.is_empty());
    }
}
