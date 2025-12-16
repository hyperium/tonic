//! Resource watcher types.

use crate::error::Error;
use crate::resource::Resource;

/// Events delivered to resource watchers.
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Indicates a new version of the resource is available.
    ResourceChanged(T),
    /// Indicates an error occurred while trying to fetch or decode the resource.
    ResourceError(Error),
    /// Indicates an error occurred after a resource has been received that should
    /// not modify the use of that resource but may provide useful information
    /// about the state of the XdsClient. The previous version of the resource
    /// should still be considered valid.
    AmbientError(Error),
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
    ///         ResourceEvent::ResourceChanged(resource) => { /* handle */}
    ///         ResourceEvent::ResourceError(error) => { /* handle */}
    ///         ResourceEvent::AmbientError(error) => { /* handle */}
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<ResourceEvent<T>> {
        todo!()
    }
}
