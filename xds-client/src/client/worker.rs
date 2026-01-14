//! ADS worker that manages the xDS stream.
//!
//! The worker runs as a background task, managing:
//! - The ADS stream lifecycle (connection, reconnection)
//! - Resource subscriptions and version/nonce tracking
//! - Dispatching resources to watchers
//! - ACK/NACK protocol

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, SinkExt, StreamExt};
use uuid::Uuid;

use crate::client::watch::{ProcessingDone, ResourceEvent};
use crate::codec::XdsCodec;
use crate::error::{Error, Result};
use crate::message::{DiscoveryRequest, DiscoveryResponse, ErrorDetail, Node};
use crate::resource::{DecodedResource, DecoderFn};
use crate::runtime::Runtime;
use crate::transport::{Transport, TransportStream};

/// Unique identifier for a watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatcherId(Uuid);

impl WatcherId {
    /// Create a new unique watcher ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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
    },
    /// Unsubscribe a watcher.
    Unwatch {
        /// The watcher to remove.
        watcher_id: WatcherId,
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
}

impl std::fmt::Debug for TypeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeState")
            .field("decoder", &"<decoder fn>")
            .field("version_info", &self.version_info)
            .field("nonce", &self.nonce)
            .field("watchers", &self.watchers)
            .field("subscription", &self.subscription)
            .finish()
    }
}

impl TypeState {
    fn new(decoder: DecoderFn) -> Self {
        Self {
            decoder,
            version_info: String::new(),
            nonce: String::new(),
            watchers: HashMap::new(),
            subscription: SubscriptionMode::Named(HashSet::new()),
        }
    }

    /// Recalculate subscription mode from watchers.
    fn recalculate_subscriptions(&mut self) {
        let has_wildcard = self.watchers.values().any(|entry| entry.name.is_empty());

        if has_wildcard {
            self.subscription = SubscriptionMode::Wildcard;
        } else {
            let names: HashSet<String> = self
                .watchers
                .values()
                .map(|entry| entry.name.clone())
                .collect();
            self.subscription = SubscriptionMode::Named(names);
        }
    }

    /// Get resource names to send in DiscoveryRequest.
    fn resource_names_for_request(&self) -> Vec<String> {
        self.subscription.resource_names_for_request()
    }
}

/// Per-watcher state.
#[derive(Debug)]
struct WatcherEntry {
    /// Channel to send events to this watcher.
    event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
    /// Resource name this watcher subscribed to (empty = wildcard).
    name: String,
}

/// Configuration for the worker.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Initial backoff duration for reconnection.
    /// Default: 1 second.
    pub initial_backoff: Duration,
    /// Maximum backoff duration for reconnection.
    /// Default: 30 seconds.
    pub max_backoff: Duration,
    /// Backoff multiplier for exponential backoff.
    /// Default: 2.0.
    pub backoff_multiplier: f64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

/// The ADS worker manages the xDS stream and dispatches resources to watchers.
pub(crate) struct AdsWorker<T, C, R> {
    /// Transport for creating streams.
    transport: T,
    /// Codec for encoding/decoding messages.
    codec: C,
    /// Runtime for spawning tasks and sleeping.
    runtime: R,
    /// Node identification.
    node: Option<Node>,
    /// Worker configuration.
    config: WorkerConfig,

    /// Receiver for commands from XdsClient.
    command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    /// Per-type_url state.
    type_states: HashMap<String, TypeState>,

    /// Current backoff duration for reconnection.
    current_backoff: Duration,
}

impl<T, C, R> AdsWorker<T, C, R>
where
    T: Transport,
    C: XdsCodec,
    R: Runtime,
{
    /// Create a new worker.
    pub(crate) fn new(
        transport: T,
        codec: C,
        runtime: R,
        node: Option<Node>,
        config: WorkerConfig,
        command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    ) -> Self {
        Self {
            transport,
            codec,
            runtime,
            node,
            config: config.clone(),
            command_rx,
            type_states: HashMap::new(),
            current_backoff: config.initial_backoff,
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
                match self.command_rx.next().await {
                    Some(cmd) => self.handle_command_disconnected(cmd),
                    None => return,
                }
            }

            // Nonce are tied to the stream
            for type_state in self.type_states.values_mut() {
                type_state.nonce.clear();
            }

            let stream = match self
                .transport
                .new_stream(self.build_initial_requests())
                .await
            {
                Ok(s) => {
                    self.current_backoff = self.config.initial_backoff;
                    s
                }
                Err(_) => {
                    self.runtime.sleep(self.current_backoff).await;
                    self.current_backoff = std::cmp::min(
                        Duration::from_secs_f64(
                            self.current_backoff.as_secs_f64() * self.config.backoff_multiplier,
                        ),
                        self.config.max_backoff,
                    );
                    continue;
                }
            };

            if self.run_connected(stream).await {
                return; // shutdown
            }
            // else: reconnect
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
                node: self.node.clone(),
                type_url: type_url.clone(),
                resource_names,
                version_info: type_state.version_info.clone(),
                response_nonce: String::new(), // Initial request has empty nonce
                error_detail: None,
            };

            if let Ok(bytes) = self.codec.encode_request(&request) {
                requests.push(bytes);
            }
        }

        requests
    }

    /// Handle a command while disconnected (just update state, can't send requests).
    fn handle_command_disconnected(&mut self, cmd: WorkerCommand) {
        match cmd {
            WorkerCommand::Watch {
                type_url,
                name,
                watcher_id,
                event_tx,
                decoder,
            } => {
                self.add_watcher(type_url, name, watcher_id, event_tx, decoder);
            }
            WorkerCommand::Unwatch { watcher_id } => {
                self.remove_watcher(watcher_id);
            }
        }
    }

    /// Run the main event loop while connected.
    ///
    /// Returns `true` if the worker should shut down, `false` to reconnect.
    async fn run_connected<S: TransportStream>(&mut self, mut stream: S) -> bool {
        loop {
            futures::select! {
                result = stream.recv().fuse() => {
                    match result {
                        Ok(Some(bytes)) => {
                            if let Err(_e) = self.handle_response(&mut stream, bytes).await {
                                return false;
                            }
                        }
                        Ok(None) => return false,
                        Err(_e) => return false,
                    }
                }

                cmd = self.command_rx.next() => {
                    match cmd {
                        Some(cmd) => {
                            if let Err(_e) = self.handle_command(&mut stream, cmd).await {
                                return false;
                            }
                        }
                        None => return true,
                    }
                }
            }
        }
    }

    /// Handle a command while connected.
    async fn handle_command<S: TransportStream>(
        &mut self,
        stream: &mut S,
        cmd: WorkerCommand,
    ) -> Result<()> {
        match cmd {
            WorkerCommand::Watch {
                type_url,
                name,
                watcher_id,
                event_tx,
                decoder,
            } => {
                self.handle_watch(stream, type_url, name, watcher_id, event_tx, decoder)
                    .await
            }
            WorkerCommand::Unwatch { watcher_id } => self.handle_unwatch(stream, watcher_id).await,
        }
    }

    /// Handle a Watch command.
    async fn handle_watch<S: TransportStream>(
        &mut self,
        stream: &mut S,
        type_url: &'static str,
        name: String,
        watcher_id: WatcherId,
        event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
        decoder: DecoderFn,
    ) -> Result<()> {
        let type_url_string = type_url.to_string();
        let is_new_type = !self.type_states.contains_key(&type_url_string);
        let subscriptions_changed = self.add_watcher(type_url, name, watcher_id, event_tx, decoder);

        if is_new_type || subscriptions_changed {
            self.send_request(stream, &type_url_string).await?;
        }

        Ok(())
    }

    /// Handle an Unwatch command.
    async fn handle_unwatch<S: TransportStream>(
        &mut self,
        stream: &mut S,
        watcher_id: WatcherId,
    ) -> Result<()> {
        if let Some((type_url, subscriptions_changed)) = self.remove_watcher(watcher_id) {
            if subscriptions_changed {
                self.send_request(stream, &type_url).await?;
            }
        }
        Ok(())
    }

    /// Add a watcher to the state.
    /// Returns true if subscriptions changed (need to send new request).
    fn add_watcher(
        &mut self,
        type_url: &'static str,
        name: String,
        watcher_id: WatcherId,
        event_tx: mpsc::Sender<ResourceEvent<DecodedResource>>,
        decoder: DecoderFn,
    ) -> bool {
        let type_url_string = type_url.to_string();
        let type_state = self
            .type_states
            .entry(type_url_string)
            .or_insert_with(|| TypeState::new(decoder));

        let old_subscription = type_state.subscription.clone();

        type_state
            .watchers
            .insert(watcher_id, WatcherEntry { event_tx, name });
        type_state.recalculate_subscriptions();

        type_state.subscription != old_subscription
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
        }

        Some((type_url, subscriptions_changed))
    }

    /// Send a DiscoveryRequest for a type.
    async fn send_request<S: TransportStream>(&self, stream: &mut S, type_url: &str) -> Result<()> {
        let type_state = match self.type_states.get(type_url) {
            Some(s) => s,
            None => return Ok(()),
        };

        let request = DiscoveryRequest {
            node: self.node.clone(),
            type_url: type_url.to_string(),
            resource_names: type_state.resource_names_for_request(),
            version_info: type_state.version_info.clone(),
            response_nonce: type_state.nonce.clone(),
            error_detail: None,
        };

        let bytes = self.codec.encode_request(&request)?;
        stream.send(bytes).await
    }

    /// Handle a response from the server.
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

        let mut decoded_resources = Vec::new();
        let mut decode_errors = Vec::new();

        for resource_any in &response.resources {
            match decoder(resource_any.value.clone()) {
                Ok(decoded) => {
                    decoded_resources.push(decoded);
                }
                Err(e) => {
                    decode_errors.push(format!("{}: {}", resource_any.type_url, e));
                }
            }
        }

        if let Some(type_state) = self.type_states.get_mut(&type_url) {
            type_state.nonce = response.nonce.clone();
        }

        if !decode_errors.is_empty() {
            self.send_nack(stream, &response, decode_errors.join("; "))
                .await?;
            self.notify_watchers_error(&type_url, Error::Validation(decode_errors.join("; ")))
                .await;
            return Ok(());
        }

        let processing_done_futures = self.dispatch_resources(&type_url, decoded_resources).await;

        for rx in processing_done_futures {
            let _ = rx.await;
        }

        if let Some(ts) = self.type_states.get_mut(&type_url) {
            ts.version_info = response.version_info.clone();
        }

        self.send_ack(stream, &response).await
    }

    /// Dispatch decoded resources to watchers.
    ///
    /// Returns futures that resolve when watchers signal ProcessingDone.
    /// Uses backpressure: waits if a watcher's channel is full.
    async fn dispatch_resources(
        &mut self,
        type_url: &str,
        resources: Vec<DecodedResource>,
    ) -> Vec<oneshot::Receiver<()>> {
        let mut processing_done_futures = Vec::new();

        let watcher_info: Vec<_> = match self.type_states.get(type_url) {
            Some(s) => s
                .watchers
                .iter()
                .map(|(id, entry)| (*id, entry.event_tx.clone(), entry.name.clone()))
                .collect(),
            None => return processing_done_futures,
        };

        for resource in resources {
            let resource_name = resource.name().to_string();
            let resource = Arc::new(resource);

            for (_watcher_id, mut event_tx, watcher_name) in watcher_info.clone() {
                // Watcher is interested if:
                // - It's a wildcard watcher (empty name), or
                // - Its name matches the resource name
                if watcher_name.is_empty() || watcher_name == resource_name {
                    let (done, rx) = ProcessingDone::channel();
                    let event = ResourceEvent::ResourceChanged {
                        resource: Arc::clone(&resource),
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

    /// Notify watchers of an error.
    ///
    /// Uses backpressure: waits if a watcher's channel is full.
    async fn notify_watchers_error(&mut self, type_url: &str, error: Error) {
        let watcher_senders: Vec<_> = match self.type_states.get(type_url) {
            Some(s) => s.watchers.values().map(|e| e.event_tx.clone()).collect(),
            None => return,
        };

        for mut event_tx in watcher_senders {
            let (done, _rx) = ProcessingDone::channel();
            let event = ResourceEvent::ResourceError {
                error: Error::Validation(error.to_string()),
                done,
            };
            let _ = event_tx.send(event).await;
        }
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

        let request = DiscoveryRequest {
            node: None, // Only send node on first request per xDS protocol
            type_url: response.type_url.clone(),
            resource_names: type_state.resource_names_for_request(),
            version_info: response.version_info.clone(),
            response_nonce: response.nonce.clone(),
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

        let request = DiscoveryRequest {
            node: None,
            type_url: response.type_url.clone(),
            resource_names: type_state.resource_names_for_request(),
            version_info: type_state.version_info.clone(), // Keep old version for NACK
            response_nonce: response.nonce.clone(),
            error_detail: Some(ErrorDetail {
                code: 3, // INVALID_ARGUMENT
                message: error_message,
            }),
        };

        let bytes = self.codec.encode_request(&request)?;
        stream.send(bytes).await
    }
}
