//! ADS worker that manages the xDS stream.
//!
//! The worker runs as a background task, managing:
//! - The ADS stream lifecycle (connection, reconnection)
//! - Resource subscriptions and version/nonce tracking
//! - Dispatching resources to watchers
//! - ACK/NACK protocol

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::client::config::{ClientConfig, ServerConfig};
use crate::client::retry::Backoff;
use crate::client::watch::{ProcessingDone, ResourceEvent};
use crate::codec::XdsCodec;
use crate::error::{Error, Result};
use crate::message::{DiscoveryRequest, DiscoveryResponse, ErrorDetail, Node};
use crate::resource::{DecodedResource, DecoderFn};
use crate::runtime::Runtime;
use crate::transport::{Transport, TransportBuilder, TransportStream};

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

/// Commands sent from `XdsClient` to the worker.
pub(crate) enum WorkerCommand {
    /// Subscribe to a resource.
    Watch {
        /// The type URL of the resource.
        type_url: &'static str,
        /// The resource name (empty string for wildcard subscription).
        name: String,
        /// Unique identifier for this watcher.
        watcher_id: WatcherId,
        /// Channel to send resource events to the watcher.
        event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
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
    },
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
}

/// State of a cached resource per gRFC A88.
#[derive(Debug, Clone)]
enum ResourceState {
    /// Resource has been requested but not yet received.
    Requested,
    /// Resource has been successfully received and validated.
    Received,
    /// Resource validation failed. Contains the error message.
    NACKed(String),
    /// Resource does not exist (server indicated deletion or absence).
    DoesNotExist,
}

/// A cached resource entry.
#[derive(Debug, Clone)]
struct CachedResource {
    /// Current state of the resource.
    state: ResourceState,
    /// The decoded resource, if successfully received.
    /// None if state is Requested, NACKed, or DoesNotExist.
    resource: Option<Arc<DecodedResource>>,
}

impl CachedResource {
    /// Create a new cached resource in Requested state.
    fn requested() -> Self {
        Self {
            state: ResourceState::Requested,
            resource: None,
        }
    }

    /// Create a cached resource in Received state.
    fn received(resource: Arc<DecodedResource>) -> Self {
        Self {
            state: ResourceState::Received,
            resource: Some(resource),
        }
    }

    /// Create a cached resource in DoesNotExist state.
    fn does_not_exist() -> Self {
        Self {
            state: ResourceState::DoesNotExist,
            resource: None,
        }
    }

    /// Create a cached resource in NACKed state.
    fn nacked(error: String) -> Self {
        Self {
            state: ResourceState::NACKed(error),
            resource: None,
        }
    }

    /// Returns true if the resource is in Requested state (waiting for server response).
    fn is_requested(&self) -> bool {
        matches!(self.state, ResourceState::Requested)
    }

    /// Convert cached state to a ResourceEvent for notifying watchers.
    /// Returns None if state is Requested (nothing to notify yet).
    fn to_event(&self) -> Option<ResourceEvent<DecodedResource>> {
        let (done, _rx) = ProcessingDone::channel();
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
            ResourceState::NACKed(error) => Some(ResourceEvent::ResourceChanged {
                result: Err(Error::Validation(error.clone())),
                done,
            }),
            ResourceState::Requested => None,
        }
    }
}

/// Per-type_url state tracking.
struct TypeState {
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
    fn new(decoder: DecoderFn, all_resources_required_in_sotw: bool) -> Self {
        Self {
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
    fn matching_watchers(&self, name: &str) -> Vec<mpsc::Sender<ResourceEvent<DecodedResource>>> {
        self.watchers
            .values()
            .filter(|e| e.subscription.matches(name))
            .map(|e| e.event_tx.clone())
            .collect()
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
    event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
    /// What resources this watcher is subscribed to.
    subscription: WatcherSubscription,
}

/// The ADS worker manages the xDS stream and dispatches resources to watchers.
pub(crate) struct AdsWorker<TB, C, R> {
    /// Transport builder for creating transports to xDS servers.
    transport_builder: TB,
    /// Codec for encoding/decoding messages.
    codec: C,
    /// Runtime for spawning tasks and sleeping.
    runtime: R,
    /// Node identification.
    node: Node,
    /// Backoff calculator for reconnection attempts.
    backoff: Backoff,
    /// Priority-ordered list of xDS servers.
    /// Index 0 has the highest priority.
    servers: Vec<ServerConfig>,
    /// Timeout for initial resource response (gRFC A57). None = disabled.
    resource_initial_timeout: Option<Duration>,
    /// Sender for timer callback commands.
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    /// Receiver for commands from XdsClient.
    command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    /// Per-type_url state.
    type_states: HashMap<String, TypeState>,
    /// Cancellation handles for resource timers (gRFC A57).
    /// Key is (type_url, resource_name). Dropping the sender cancels the timer.
    resource_timers: HashMap<(String, String), oneshot::Sender<()>>,
}

impl<TB, C, R> AdsWorker<TB, C, R>
where
    TB: TransportBuilder,
    C: XdsCodec,
    R: Runtime,
{
    /// Create a new worker.
    pub(crate) fn new(
        transport_builder: TB,
        codec: C,
        runtime: R,
        config: ClientConfig,
        command_tx: mpsc::UnboundedSender<WorkerCommand>,
        command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    ) -> Self {
        Self {
            transport_builder,
            codec,
            runtime,
            node: config.node,
            backoff: Backoff::new(config.retry_policy),
            servers: config.servers,
            resource_initial_timeout: config.resource_initial_timeout,
            command_tx,
            command_rx,
            type_states: HashMap::new(),
            resource_timers: HashMap::new(),
        }
    }

    /// Run the worker event loop.
    ///
    /// This method runs until all `XdsClient` handles are dropped
    /// (which closes the command channel).
    pub(crate) async fn run(mut self) {
        loop {
            // Wait for at least one subscription before connecting.
            // This prevents deadlock with servers that require a message before
            // sending response headers - we need something to send.
            while self.type_states.is_empty() {
                match self.command_rx.recv().await {
                    Some(cmd) => {
                        let _ = self
                            .handle_command::<<TB::Transport as Transport>::Stream>(None, cmd)
                            .await;
                    }
                    None => return,
                }
            }

            // Nonces are tied to the stream
            for type_state in self.type_states.values_mut() {
                type_state.nonce.clear();
            }

            // Connect to server.
            // Future extension (gRFC A71): Try servers in priority order with fallback.
            let server = match self.servers.first() {
                Some(s) => s,
                None => return, // No servers configured
            };

            let transport = match self.transport_builder.build(server).await {
                Ok(t) => t,
                Err(_) => {
                    match self.backoff.next_backoff() {
                        Some(backoff) => self.runtime.sleep(backoff).await,
                        None => return, // Max attempts exceeded
                    }
                    continue;
                }
            };

            let stream = match transport.new_stream(self.build_initial_requests()).await {
                Ok(s) => {
                    self.backoff.reset();
                    s
                }
                Err(_) => {
                    match self.backoff.next_backoff() {
                        Some(backoff) => self.runtime.sleep(backoff).await,
                        None => return, // Max attempts exceeded
                    }
                    continue;
                }
            };

            match self.run_connected(stream).await {
                Ok(()) => return, // shutdown
                Err(_e) => {
                    match self.backoff.next_backoff() {
                        Some(backoff) => self.runtime.sleep(backoff).await,
                        None => return, // Max attempts exceeded
                    }
                    continue;
                }
            }
        }
    }

    /// Build initial DiscoveryRequests for all active subscriptions.
    ///
    /// These are sent when establishing the stream to prevent deadlock with
    /// servers that don't send response headers until they receive a request.
    fn build_initial_requests(&self) -> Vec<Bytes> {
        let mut requests = Vec::new();

        for (type_url, type_state) in &self.type_states {
            if type_state.watchers.is_empty() {
                continue;
            }

            let resource_names = type_state.resource_names_for_request();

            let request = DiscoveryRequest {
                node: &self.node,
                type_url,
                resource_names: &resource_names,
                version_info: &type_state.version_info,
                response_nonce: "", // Initial request has empty nonce
                error_detail: None,
            };

            if let Ok(bytes) = self.codec.encode_request(&request) {
                requests.push(bytes);
            }
        }

        requests
    }

    /// Run the main event loop while connected.
    ///
    /// Returns `Ok(())` if the worker should shut down (command channel closed).
    /// Returns `Err` if an error occurred and the worker should reconnect.
    async fn run_connected<S: TransportStream>(&mut self, mut stream: S) -> Result<()> {
        loop {
            tokio::select! {
                result = stream.recv() => {
                    match result {
                        Ok(Some(bytes)) => {
                            self.handle_response(&mut stream, bytes).await?;
                        }
                        // Stream closed by server; return Err to trigger reconnection
                        Ok(None) => return Err(Error::StreamClosed),
                        Err(e) => return Err(e),
                    }
                }

                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            self.handle_command(Some(&mut stream), cmd).await?;
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    /// Handle a command, optionally sending network requests if connected.
    ///
    /// When `stream` is `None`, only state updates are performed (disconnected mode).
    /// When `stream` is `Some`, subscription changes trigger network requests.
    async fn handle_command<S: TransportStream>(
        &mut self,
        stream: Option<&mut S>,
        cmd: WorkerCommand,
    ) -> Result<()> {
        match cmd {
            WorkerCommand::Watch {
                type_url,
                name,
                watcher_id,
                event_tx,
                decoder,
                all_resources_required_in_sotw,
            } => {
                if self.add_watcher(
                    type_url,
                    name,
                    watcher_id,
                    event_tx,
                    decoder,
                    all_resources_required_in_sotw,
                ) {
                    if let Some(stream) = stream {
                        self.send_request(stream, type_url).await?;
                    }
                }
            }
            WorkerCommand::Unwatch { watcher_id } => {
                if let Some((type_url, true)) = self.remove_watcher(watcher_id) {
                    if let Some(stream) = stream {
                        self.send_request(stream, &type_url).await?;
                    }
                }
            }
            WorkerCommand::ResourceTimerExpired { type_url, name } => {
                self.handle_resource_timeout(&type_url, &name).await;
            }
        }
        Ok(())
    }

    /// Add a watcher to the state.
    ///
    /// If the resource is already cached, the watcher receives the cached state immediately.
    /// Returns true if subscriptions changed (need to send new request to server).
    fn add_watcher(
        &mut self,
        type_url: &'static str,
        name: String,
        watcher_id: WatcherId,
        event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
        decoder: DecoderFn,
        all_resources_required_in_sotw: bool,
    ) -> bool {
        let type_url_string = type_url.to_string();
        let type_state = self
            .type_states
            .entry(type_url_string.clone())
            .or_insert_with(|| TypeState::new(decoder, all_resources_required_in_sotw));

        let old_subscription = type_state.subscription.clone();
        let watcher_subscription = WatcherSubscription::from_name(name.clone());

        // Track if we need to start a timer (resource in Requested state)
        let mut start_timer_for: Option<String> = None;

        // For named subscriptions, check cache and send cached state to new watcher.
        // For wildcard subscriptions, watchers receive updates as they come in.
        if let WatcherSubscription::Named(ref resource_name) = watcher_subscription {
            let cached = type_state
                .cache
                .entry(resource_name.clone())
                .or_insert_with(CachedResource::requested);

            if let Some(event) = cached.to_event() {
                // Send cached state to watcher (non-blocking, ignore if full)
                let _ = event_tx.try_send(event);
            }

            if cached.is_requested() {
                // Resource pending - start a timer (gRFC A57)
                start_timer_for = Some(resource_name.clone());
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

        // Start timer if resource is in Requested state
        if let (Some(resource_name), Some(timeout)) =
            (start_timer_for, self.resource_initial_timeout)
        {
            self.start_resource_timer(&type_url_string, resource_name, timeout);
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
            self.type_states.remove(&type_url);
            // Cancel all pending resource timers for this type.
            self.resource_timers.retain(|key, _| key.0 != type_url);
        }

        Some((type_url, subscriptions_changed))
    }

    /// Send a DiscoveryRequest for a type.
    async fn send_request<S: TransportStream>(&self, stream: &mut S, type_url: &str) -> Result<()> {
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
        stream.send(bytes).await
    }

    /// Handle a response from the server.
    ///
    /// Implements partial success per gRFC A46: valid resources are accepted even
    /// if some resources in the response fail validation. Each resource is processed
    /// independently:
    /// - Valid resources are cached and dispatched to watchers
    /// - Invalid resources are cached as NACKed and errors sent to specific watchers
    /// - Missing resources (for types with ALL_RESOURCES_REQUIRED_IN_SOTW) are marked deleted
    async fn handle_response<S: TransportStream>(
        &mut self,
        stream: &mut S,
        bytes: Bytes,
    ) -> Result<()> {
        let response = self.codec.decode_response(bytes)?;
        let type_url = response.type_url.clone();

        let decoder = match self.type_states.get(&type_url) {
            Some(s) => &s.decoder,
            None => {
                return Ok(());
            }
        };

        // Decode all resources, tracking valid and invalid separately.
        // Per A46, we accept valid resources even if some fail validation.
        // Per A88, we categorize errors:
        // - top_level_errors: deserialization failures where name cannot be extracted
        // - per_resource_errors: validation failures where name is known
        let mut valid_resources: Vec<DecodedResource> = Vec::new();
        let mut top_level_errors: Vec<String> = Vec::new();
        let mut per_resource_errors: Vec<(String, String)> = Vec::new(); // (name, error)

        for resource_any in &response.resources {
            match decoder(resource_any.value.clone()) {
                crate::resource::DecodeResult::Success { resource, .. } => {
                    valid_resources.push(resource);
                }
                crate::resource::DecodeResult::ResourceError { name, error } => {
                    per_resource_errors.push((name, error.to_string()));
                }
                crate::resource::DecodeResult::TopLevelError(error) => {
                    top_level_errors.push(error.to_string());
                }
            }
        }

        if let Some(type_state) = self.type_states.get_mut(&type_url) {
            type_state.nonce = response.nonce.clone();
        }

        let received_names: HashSet<String> = valid_resources
            .iter()
            .map(|r| r.name().to_string())
            .collect();

        let mut processing_done_futures = self.dispatch_resources(&type_url, valid_resources).await;

        // Only notify watchers for per-resource errors (where we know the name).
        // Top-level errors have no associated name, so no watcher to notify.
        for (resource_name, error) in &per_resource_errors {
            self.notify_resource_error(&type_url, resource_name, error)
                .await;
        }

        // Detect deleted resources (per A53):
        // For resource types with ALL_RESOURCES_REQUIRED_IN_SOTW = true,
        // any previously-received resource not in this response is deleted.
        let deleted_futures = self
            .detect_deleted_resources(&type_url, &received_names)
            .await;
        processing_done_futures.extend(deleted_futures);

        // Wait for all watchers to finish processing.
        for rx in processing_done_futures {
            let _ = rx.await;
        }

        let has_errors = !top_level_errors.is_empty() || !per_resource_errors.is_empty();
        if !has_errors {
            // Only update version on ACK; NACK must keep the old version so the
            // server knows which version the client is still running.
            if let Some(ts) = self.type_states.get_mut(&type_url) {
                ts.version_info = response.version_info.clone();
            }
            self.send_ack(stream, &response).await
        } else {
            // Build NACK message combining both error categories
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

            self.send_nack(stream, &response, error_parts.join("; "))
                .await
        }
    }

    /// Dispatch decoded resources to watchers and update cache.
    ///
    /// Returns futures that resolve when watchers signal ProcessingDone.
    /// Uses backpressure: waits if a watcher's channel is full.
    async fn dispatch_resources(
        &mut self,
        type_url: &str,
        resources: Vec<DecodedResource>,
    ) -> Vec<oneshot::Receiver<()>> {
        let mut processing_done_futures = Vec::new();

        let watcher_info: Vec<_> = match self.type_states.get_mut(type_url) {
            Some(s) => {
                for resource in &resources {
                    let resource_name = resource.name().to_string();
                    s.cache.insert(
                        resource_name,
                        CachedResource::received(Arc::new(resource.clone())),
                    );
                }
                s.watchers
                    .iter()
                    .map(|(id, entry)| (*id, entry.event_tx.clone(), entry.subscription.clone()))
                    .collect()
            }
            None => return processing_done_futures,
        };

        // Cancel resource timers for received resources (gRFC A57).
        for resource in &resources {
            self.resource_timers
                .remove(&(type_url.to_string(), resource.name().to_string()));
        }

        for resource in resources {
            let resource_name = resource.name().to_string();
            let resource = Arc::new(resource);

            for (_watcher_id, event_tx, subscription) in watcher_info.clone() {
                if subscription.matches(&resource_name) {
                    let (done, rx) = ProcessingDone::channel();
                    let event = ResourceEvent::ResourceChanged {
                        result: Ok(Arc::clone(&resource)),
                        done,
                    };
                    // Use backpressure: await if channel is full.
                    // Ignore send errors (watcher dropped).
                    let _ = event_tx.send(event).await;
                    processing_done_futures.push(rx);
                }
            }
        }

        processing_done_futures
    }

    /// Notify watchers of a validation error for a specific resource.
    ///
    /// Per gRFC A46/A88, errors are routed only to watchers interested in
    /// that specific resource (plus wildcard watchers).
    async fn notify_resource_error(&mut self, type_url: &str, resource_name: &str, error: &str) {
        let type_state = match self.type_states.get_mut(type_url) {
            Some(s) => s,
            None => return,
        };

        type_state.cache.insert(
            resource_name.to_string(),
            CachedResource::nacked(error.to_string()),
        );

        // Cancel the resource timer (gRFC A57).
        self.resource_timers
            .remove(&(type_url.to_string(), resource_name.to_string()));

        for event_tx in type_state.matching_watchers(resource_name) {
            let (done, _rx) = ProcessingDone::channel();
            let event = ResourceEvent::ResourceChanged {
                result: Err(Error::Validation(error.to_string())),
                done,
            };
            let _ = event_tx.send(event).await;
        }
    }

    /// Detect resources that were deleted (present in cache but not in response).
    ///
    /// Per gRFC A53, for resource types with ALL_RESOURCES_REQUIRED_IN_SOTW = true,
    /// if a previously-received resource is absent from a new SotW response,
    /// it is treated as deleted.
    async fn detect_deleted_resources(
        &mut self,
        type_url: &str,
        received_names: &HashSet<String>,
    ) -> Vec<oneshot::Receiver<()>> {
        let mut processing_done_futures = Vec::new();

        let type_state = match self.type_states.get_mut(type_url) {
            Some(s) => s,
            None => return processing_done_futures,
        };

        if !type_state.all_resources_required_in_sotw {
            return processing_done_futures;
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
            type_state
                .cache
                .insert(name.clone(), CachedResource::does_not_exist());

            for event_tx in type_state.matching_watchers(&name) {
                let (done, rx) = ProcessingDone::channel();
                let event = ResourceEvent::ResourceChanged {
                    result: Err(Error::ResourceDoesNotExist),
                    done,
                };
                let _ = event_tx.send(event).await;
                processing_done_futures.push(rx);
            }
        }

        processing_done_futures
    }

    /// Send an ACK for a response.
    async fn send_ack<S: TransportStream>(
        &self,
        stream: &mut S,
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
        stream.send(bytes).await
    }

    /// Send a NACK for a response.
    async fn send_nack<S: TransportStream>(
        &self,
        stream: &mut S,
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
        stream.send(bytes).await
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
        let type_url_owned = type_url.to_string();
        let command_tx = self.command_tx.clone();
        let runtime = self.runtime.clone();

        self.runtime.spawn(async move {
            tokio::select! {
                _ = runtime.sleep(timeout) => {
                    let _ = command_tx.send(WorkerCommand::ResourceTimerExpired {
                        type_url: type_url_owned,
                        name,
                    });
                }
                _ = cancel_rx => {}
            }
        });

        self.resource_timers.insert(key, cancel_tx);
    }

    /// Handle a resource timer expiration (gRFC A57).
    ///
    /// If the resource is still in Requested state, marks it as DoesNotExist
    /// and notifies all watchers interested in this resource.
    async fn handle_resource_timeout(&mut self, type_url: &str, name: &str) {
        self.resource_timers
            .remove(&(type_url.to_string(), name.to_string()));

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
            .insert(name.to_string(), CachedResource::does_not_exist());

        for event_tx in type_state.matching_watchers(name) {
            let (done, _rx) = ProcessingDone::channel();
            let event = ResourceEvent::ResourceChanged {
                result: Err(Error::ResourceDoesNotExist),
                done,
            };
            let _ = event_tx.send(event).await;
        }
    }
}
