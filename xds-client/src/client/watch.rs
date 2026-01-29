//! Resource watcher types.

use std::marker::PhantomData;
use std::sync::Arc;

use futures::channel::{mpsc, oneshot};
use futures::StreamExt;

use crate::client::worker::{WatcherId, WorkerCommand};
use crate::error::Error;
use crate::resource::{DecodedResource, Resource};

/// A signal to indicate that processing of a resource event is complete.
///
/// The xDS client waits for this signal before sending ACK/NACK to the server.
/// This allows watchers to add cascading subscriptions (e.g. LDS -> RDS -> CDS -> EDS)
/// that will be included in the same ACK.
///
/// # Automatic Signaling
///
/// Signals automatically when dropped. If you have cascading watches to add, simply
/// add them before dropping the `ProcessingDone`.
///
/// # Example
///
/// ```ignore
/// match event {
///     ResourceEvent::ResourceChanged { resource, done } => {
///         // Process the resource, possibly add cascading watches.
///         client.watch::<RouteConfiguration>(&resource.route_name());
///         // Signal is sent automatically when done is dropped
///     }
///     ResourceEvent::ResourceError { error, done } => {
///         eprintln!("Error: {}", error);
///         // Signal is sent automatically when done is dropped
///     }
///     ResourceEvent::AmbientError { error, .. } => {
///         eprintln!("Ambient error: {}", error);
///         // Signal is sent automatically when done is dropped
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ProcessingDone(Option<oneshot::Sender<()>>);

impl ProcessingDone {
    /// Create a channel pair for signaling.
    ///
    /// Returns the `ProcessingDone` sender and a receiver future that resolves
    /// when the sender is dropped.
    pub(crate) fn channel() -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(tx)), rx)
    }
}

impl Drop for ProcessingDone {
    fn drop(&mut self) {
        // Auto-signal on drop to prevent deadlocks.
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// Events delivered to resource watchers.
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Indicates a new version of the resource is available.
    ///
    /// The resource is wrapped in `Arc` because multiple watchers may
    /// subscribe to the same resource and share the same data.
    ResourceChanged {
        /// The updated resource, shared via `Arc`.
        resource: Arc<T>,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
    /// Indicates an error occurred while trying to fetch or decode the resource.
    ResourceError {
        /// The error that occurred.
        error: Error,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
    /// Indicates an error occurred after a resource has been received that should
    /// not modify the use of that resource but may provide useful information
    /// about the state of the XdsClient. The previous version of the resource
    /// should still be considered valid.
    AmbientError {
        /// The error that occurred.
        error: Error,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
}

/// A watcher for resources of type `T`.
///
/// Call [`next()`](Self::next) to receive resource events.
/// Dropping the watcher unsubscribes from the resource.
#[derive(Debug)]
pub struct ResourceWatcher<T: Resource> {
    /// Channel to receive events from the worker.
    event_rx: mpsc::Receiver<ResourceEvent<DecodedResource>>,
    /// Unique identifier for this watcher.
    watcher_id: WatcherId,
    /// Channel to send commands to the worker (for unwatch on drop).
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
    /// Marker for the resource type.
    _marker: PhantomData<T>,
}

impl<T: Resource> ResourceWatcher<T> {
    /// Create a new resource watcher.
    pub(crate) fn new(
        event_rx: mpsc::Receiver<ResourceEvent<DecodedResource>>,
        watcher_id: WatcherId,
        command_tx: mpsc::UnboundedSender<WorkerCommand>,
    ) -> Self {
        Self {
            event_rx,
            watcher_id,
            command_tx,
            _marker: PhantomData,
        }
    }

    /// Returns the next resource event.
    ///
    /// Returns `None` when the subscription is closed (worker shut down).
    ///
    /// # Example
    ///
    /// ```ignore
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::ResourceChanged { resource, done } => {
    ///             // Process the resource, possibly add cascading watches.
    ///             client.watch::<RouteConfiguration>(&resource.route_name());
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::ResourceError { error, done } => {
    ///             eprintln!("Error: {}", error);
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             eprintln!("Ambient error: {}", error);
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<ResourceEvent<T>> {
        let event = self.event_rx.next().await?;

        Some(match event {
            ResourceEvent::ResourceChanged { resource, done } => match resource.downcast::<T>() {
                Some(typed_resource) => ResourceEvent::ResourceChanged {
                    resource: typed_resource,
                    done,
                },
                None => ResourceEvent::ResourceError {
                    error: Error::Validation(format!(
                        "resource type mismatch (expected: {}, actual: {})",
                        std::any::type_name::<T>(),
                        resource.type_url()
                    )),
                    done,
                },
            },
            ResourceEvent::ResourceError { error, done } => {
                ResourceEvent::ResourceError { error, done }
            }
            ResourceEvent::AmbientError { error, done } => {
                ResourceEvent::AmbientError { error, done }
            }
        })
    }
}

impl<T: Resource> Drop for ResourceWatcher<T> {
    fn drop(&mut self) {
        let _ = self.command_tx.unbounded_send(WorkerCommand::Unwatch {
            watcher_id: self.watcher_id,
        });
    }
}
