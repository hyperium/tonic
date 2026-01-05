//! Resource watcher types.

use crate::error::Error;
use crate::resource::Resource;
use futures_channel::oneshot;

/// A signal to indicate that processing of a resource event is complete.
///
/// The xDS client waits for this signal before sending ACK/NACK to the server.
/// This allows watchers to add cascading subscriptions (e.g. LDS -> RDS -> CDS -> EDS)
/// that will be included in the same ACK.
///
/// # Automatic Signaling
///
/// Signals automatically when dropped, so you don't need to call [`.complete()`](Self::complete)
/// explicitly if you have no cascading watches to add.
///
/// # Example
///
/// ```ignore
/// match event {
///     ResourceEvent::ResourceChanged { resource, done } => {
///         // Process the resource, possibly add cascading watches.
///         client.watch::<RouteConfiguration>(&resource.route_name());
///         done.complete();
///     }
///     ResourceEvent::ResourceError { error, done } => {
///         eprintln!("Error: {}", error);
///         done.complete();
///     }
///     ResourceEvent::AmbientError { error, .. } => {
///         // Can also rely on auto-signal on drop
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
    /// when `complete()` is called or the sender is dropped.
    #[allow(dead_code)] // TODO: remove once used by XdsClient worker
    pub(crate) fn channel() -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(tx)), rx)
    }

    /// Signal that processing is complete.
    ///
    /// This is equivalent to dropping the `ProcessingDone`, but more explicit.
    pub fn complete(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for ProcessingDone {
    fn drop(&mut self) {
        // Auto-signal on drop to prevent deadlocks if the caller forgets
        // or doesn't need to explicitly signal.
        self.complete();
    }
}

/// Events delivered to resource watchers.
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Indicates a new version of the resource is available.
    ResourceChanged {
        /// The updated resource.
        resource: T,
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
    // TODO: replace with proper implementation
    _marker: std::marker::PhantomData<T>,
}

impl<T: Resource> ResourceWatcher<T> {
    /// Returns the next resource event.
    ///
    /// Returns `None` when the subscription is closed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// while let Some(event) = watcher.next().await {
    ///     match event {
    ///         ResourceEvent::ResourceChanged { resource, done } => {
    ///             // Process the resource, possibly add cascading watches.
    ///             client.watch::<RouteConfiguration>(&resource.route_name());
    ///             done.complete();
    ///         }
    ///         ResourceEvent::ResourceError { error, done } => {
    ///             eprintln!("Error: {}", error);
    ///             done.complete();
    ///         }
    ///         ResourceEvent::AmbientError { error, .. } => {
    ///             // Can also rely on auto-signal on drop
    ///             eprintln!("Ambient error: {}", error);
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<ResourceEvent<T>> {
        todo!()
    }
}
