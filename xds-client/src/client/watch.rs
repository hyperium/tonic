//! Resource watcher types.

use crate::error::Error;
use crate::resource::Resource;
use std::future::Future;

/// Events delivered to resource watchers.
#[derive(Debug)]
pub enum ResourceEvent<T> {
    /// Resource was added or updated.
    Upsert(T),
    /// Resource was removed.
    Removed {
        /// Resource name.
        name: String,
    },
    /// Error occurred for this resource type.
    Error(Error),
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
    ///         ResourceEvent::Upsert(resource) => { /* handle */}
    ///         ResourceEvent::Removed { name } => { /* handle */}
    ///         ResourceEvent::Error(error) => { /* handle */}
    ///     }
    /// }
    /// ```
    pub fn next(&mut self) -> impl Future<Output = Option<ResourceEvent<T>>> + '_ {
        async { todo!() }
    }
}
