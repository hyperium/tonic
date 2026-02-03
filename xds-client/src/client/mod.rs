//! Client interface through which the user can watch and receive updates for xDS resources.

use futures::channel::mpsc;

use crate::client::config::ClientConfig;
use crate::client::watch::ResourceWatcher;
use crate::client::worker::{AdsWorker, WatcherId, WorkerCommand};
use crate::codec::XdsCodec;
use crate::resource::{DecodedResource, DecoderFn, Resource};
use crate::runtime::Runtime;
use crate::transport::TransportBuilder;

pub mod config;
pub mod retry;
pub mod watch;
pub mod worker;

/// Builder for [`XdsClient`].
#[derive(Debug)]
pub struct XdsClientBuilder<TB, C, R> {
    config: ClientConfig,
    transport_builder: TB,
    codec: C,
    runtime: R,
}

impl<TB, C, R> XdsClientBuilder<TB, C, R>
where
    TB: TransportBuilder,
    C: XdsCodec,
    R: Runtime,
{
    /// Create a new builder with the given configuration, transport builder, codec, and runtime.
    pub fn new(config: ClientConfig, transport_builder: TB, codec: C, runtime: R) -> Self {
        Self {
            config,
            transport_builder,
            codec,
            runtime,
        }
    }

    /// Build the client and start the background worker.
    ///
    /// This spawns a background task that manages the ADS stream.
    /// The task runs until all `XdsClient` handles are dropped.
    pub fn build(self) -> XdsClient {
        let (command_tx, command_rx) = mpsc::unbounded();

        let worker = AdsWorker::new(
            self.transport_builder,
            self.codec,
            self.runtime.clone(),
            self.config,
            command_tx.clone(),
            command_rx,
        );

        self.runtime.spawn(async move {
            worker.run().await;
        });

        XdsClient { command_tx }
    }
}

/// The xDS client.
///
/// This is a handle to the background worker that manages the ADS stream.
/// Cloning this handle creates a new reference to the same worker.
///
/// When all `XdsClient` handles are dropped, the background worker shuts down.
#[derive(Clone, Debug)]
pub struct XdsClient {
    /// Channel to send commands to the worker.
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
}

/// Default buffer size for watcher event channels.
///
/// This provides backpressure when watchers are slow to process events.
const WATCHER_CHANNEL_BUFFER_SIZE: usize = 16;

impl XdsClient {
    /// Create a new builder with the given configuration, transport builder, codec, and runtime.
    pub fn builder<TB, C, R>(
        config: ClientConfig,
        transport_builder: TB,
        codec: C,
        runtime: R,
    ) -> XdsClientBuilder<TB, C, R>
    where
        TB: TransportBuilder,
        C: XdsCodec,
        R: Runtime,
    {
        XdsClientBuilder::new(config, transport_builder, codec, runtime)
    }

    /// Watch a resource by name.
    ///
    /// Returns a [`ResourceWatcher`] that receives events for this resource.
    /// Dropping the watcher automatically unsubscribes.
    ///
    /// # Arguments
    ///
    /// * `name` - The resource name to watch. Use an empty string for wildcard
    ///   subscriptions (receive all resources of this type).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut watcher = client.watch::<Listener>("my-listener");
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::ResourceChanged { resource, done } => {
    ///             println!("Listener changed: {}", resource.name());
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::ResourceError { error, done } => {
    ///             println!("Error watching listener: {}", error);
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             println!("Ambient error: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    pub fn watch<T: Resource>(&self, name: impl Into<String>) -> ResourceWatcher<T> {
        let name = name.into();
        let watcher_id = WatcherId::new();
        let (event_tx, event_rx) = mpsc::channel(WATCHER_CHANNEL_BUFFER_SIZE);

        let decoder: DecoderFn = Box::new(|bytes| match crate::resource::decode::<T>(bytes) {
            crate::resource::DecodeResult::Success { name, resource } => {
                crate::resource::DecodeResult::Success {
                    name: name.clone(),
                    resource: DecodedResource::new(name, resource),
                }
            }
            crate::resource::DecodeResult::ResourceError { name, error } => {
                crate::resource::DecodeResult::ResourceError { name, error }
            }
            crate::resource::DecodeResult::TopLevelError(error) => {
                crate::resource::DecodeResult::TopLevelError(error)
            }
        });

        let _ = self.command_tx.unbounded_send(WorkerCommand::Watch {
            type_url: T::TYPE_URL.as_str(),
            name,
            watcher_id,
            event_tx,
            decoder,
            all_resources_required_in_sotw: T::ALL_RESOURCES_REQUIRED_IN_SOTW,
        });

        ResourceWatcher::new(event_rx, watcher_id, self.command_tx.clone())
    }
}
