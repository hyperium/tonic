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
///     ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
///         // Process the new resource, possibly add cascading watches.
///         client.watch::<RouteConfiguration>(&resource.route_name());
///         // Signal is sent automatically when done is dropped
///     }
///     ResourceEvent::ResourceChanged { result: Err(error), done } => {
///         // Resource was invalidated (validation error or deleted)
///         eprintln!("Resource invalidated: {}", error);
///         // Stop using the previously cached resource
///     }
///     ResourceEvent::AmbientError { error, .. } => {
///         // Non-fatal error, continue using cached resource
///         eprintln!("Ambient error: {}", error);
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
///
/// Per gRFC A88, there are two types of events:
/// - `ResourceChanged`: Indicates a change in the resource's cached state
/// - `AmbientError`: Non-fatal errors that don't affect the cached resource
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Indicates a change in the resource's cached state.
    ///
    /// This event is sent when:
    /// - A new valid resource is received (`Ok(resource)`)
    /// - A validation error occurred (`Err(Error::Validation(...))`)
    /// - The resource was deleted or doesn't exist (`Err(Error::ResourceDoesNotExist)`)
    ///
    /// When `result` is `Err`, the previously cached resource (if any) should be
    /// invalidated. The watcher should stop using the old resource data.
    ///
    /// The resource is wrapped in `Arc` because multiple watchers may
    /// subscribe to the same resource and share the same data.
    ResourceChanged {
        /// The result of the resource update.
        /// - `Ok(resource)`: New valid resource received
        /// - `Err(error)`: Cache-invalidating error (validation failure, does not exist)
        result: Result<Arc<T>, Error>,
        /// Signal when processing is complete.
        done: ProcessingDone,
    },
    /// Indicates a non-fatal error that doesn't affect the cached resource.
    ///
    /// This is sent for transient errors like temporary connectivity issues
    /// with the xDS management server. The previously cached resource (if any)
    /// should continue to be used.
    ///
    /// Per gRFC A88, ambient errors should not cause the client to stop using
    /// a previously valid resource.
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
    ///         ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
    ///             // Process the new resource, possibly add cascading watches.
    ///             client.watch::<RouteConfiguration>(&resource.route_name());
    ///             // Signal is sent automatically when done is dropped
    ///         }
    ///         ResourceEvent::ResourceChanged { result: Err(error), done } => {
    ///             // Resource was invalidated (validation error or deleted)
    ///             eprintln!("Resource invalidated: {}", error);
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             // Non-fatal error, continue using cached resource
    ///             eprintln!("Ambient error: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<ResourceEvent<T>> {
        let event = self.event_rx.next().await?;

        Some(match event {
            ResourceEvent::ResourceChanged { result, done } => {
                let typed_result = match result {
                    Ok(resource) => match resource.downcast::<T>() {
                        Some(typed_resource) => Ok(typed_resource),
                        None => Err(Error::Validation(format!(
                            "resource type mismatch (expected: {}, actual: {})",
                            std::any::type_name::<T>(),
                            resource.type_url()
                        ))),
                    },
                    Err(e) => Err(e),
                };
                ResourceEvent::ResourceChanged {
                    result: typed_result,
                    done,
                }
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
