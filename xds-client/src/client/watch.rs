//! Resource watcher types.

use crate::error::Error;
use crate::resource::Resource;
use std::pin::Pin;
use std::task::{Context, Poll};
use futures::Stream;

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
/// Implements [`Stream`] to receive resource events.
/// Dropping the watcher unsubscribes from the resource.
#[derive(Debug)]
pub struct ResourceWatcher<T: Resource> {
    // TODO: replace with proper implementation
    _marker: std::marker::PhantomData<T>,
}


impl<T: Resource> Stream for ResourceWatcher<T> {
    type Item = ResourceEvent<T>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}