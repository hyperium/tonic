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

//! Client interface through which the user can watch and receive updates for xDS resources.

use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::client::config::ClientConfig;
use crate::client::watch::ResourceWatcher;
use crate::client::worker::{AdsWorker, WatcherId, WorkerCommand};
use crate::codec::XdsCodec;
use crate::metrics::MetricsRecorder;
use crate::resource::{DecodedResource, DecoderFn, Resource};
use crate::runtime::Runtime;
use crate::transport::TransportBuilder;

pub mod config;
pub mod retry;
pub mod watch;
pub mod worker;

/// Builder for [`XdsClient`].
pub struct XdsClientBuilder<TB, C, R> {
    config: ClientConfig,
    transport_builder: TB,
    codec: C,
    runtime: R,
    recorder: Option<Arc<dyn MetricsRecorder>>,
}

impl<TB: fmt::Debug, C: fmt::Debug, R: fmt::Debug> fmt::Debug for XdsClientBuilder<TB, C, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XdsClientBuilder")
            .field("config", &self.config)
            .field("transport_builder", &self.transport_builder)
            .field("codec", &self.codec)
            .field("runtime", &self.runtime)
            .field(
                "recorder",
                &self
                    .recorder
                    .as_ref()
                    .map(|_| "Some(Arc<dyn MetricsRecorder>)")
                    .unwrap_or("None"),
            )
            .finish()
    }
}

impl<TB, C, R> XdsClientBuilder<TB, C, R>
where
    TB: TransportBuilder,
    C: XdsCodec,
    R: Runtime,
{
    /// Create a new builder with the given configuration, transport builder, codec, and runtime.
    ///
    /// No metrics recorder is configured by default; the worker skips all A78
    /// metric emission. Configure a backend with
    /// [`with_metrics_recorder`](Self::with_metrics_recorder) to receive measurements.
    pub fn new(config: ClientConfig, transport_builder: TB, codec: C, runtime: R) -> Self {
        Self {
            config,
            transport_builder,
            codec,
            runtime,
            recorder: None,
        }
    }

    /// Set the metrics recorder.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::sync::Arc;
    /// use xds_client::MetricsRecorder;
    ///
    /// let recorder: Arc<dyn MetricsRecorder> = Arc::new(MyOtelRecorder::new());
    /// let builder = XdsClient::builder(config, transport, codec, runtime)
    ///     .with_metrics_recorder(recorder);
    /// ```
    pub fn with_metrics_recorder(mut self, recorder: Arc<dyn MetricsRecorder>) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// Build the client and start the background worker.
    ///
    /// This spawns a background task that manages the ADS stream.
    /// The task runs until all `XdsClient` handles are dropped.
    pub fn build(self) -> XdsClient {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_BUFFER_SIZE);

        let worker = AdsWorker::new(
            self.transport_builder,
            self.codec,
            self.runtime.clone(),
            self.config,
            command_tx.clone(),
            command_rx,
            self.recorder,
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
    command_tx: mpsc::Sender<WorkerCommand>,
}

/// Buffer size for the command channel between [`XdsClient`] handles and the worker.
///
/// Commands are lightweight (watch/unwatch/timer), so a modest buffer suffices.
/// The channel provides backpressure if the worker is temporarily busy processing
/// a response.
const COMMAND_CHANNEL_BUFFER_SIZE: usize = 64;

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
    /// let mut watcher = client.watch::<Listener>("my-listener").await;
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
    ///             println!("Listener changed: {}", resource.name());
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::ResourceChanged { result: Err(error), .. } => {
    ///             println!("Error watching listener: {}", error);
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             println!("Ambient error: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn watch<T: Resource>(&self, name: impl Into<String>) -> ResourceWatcher<T> {
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

        let _ = self
            .command_tx
            .send(WorkerCommand::Watch {
                type_url: T::TYPE_URL.as_str(),
                name,
                watcher_id,
                event_tx,
                decoder,
                all_resources_required_in_sotw: T::ALL_RESOURCES_REQUIRED_IN_SOTW,
            })
            .await;

        ResourceWatcher::new(event_rx, watcher_id, self.command_tx.clone())
    }

    /// Creates a disconnected client with no backing worker.
    ///
    /// `watch()` calls will succeed but the returned watchers immediately
    /// yield `None` (the worker receiver is dropped).
    ///
    /// Requires the `test-util` feature.
    #[cfg(feature = "test-util")]
    pub fn disconnected() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self { command_tx: tx }
    }
}
