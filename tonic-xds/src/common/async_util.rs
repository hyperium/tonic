//! Utilities for async operations.

use std::future::Future;
use std::pin::Pin;

pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A [`tokio::task::JoinHandle`] wrapper that aborts the task when dropped.
pub(crate) struct AbortOnDrop(pub(crate) tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
