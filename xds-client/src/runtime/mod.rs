//! Provides abstraction for async runtimes.

use std::future::Future;
use std::time::Duration;

#[cfg(feature = "rt-tokio")]
pub mod tokio;

/// Trait for async runtime operations.
///
/// This abstraction allows the xDS client to be runtime-agnostic.
// TODO: unify with the grpc-rust runtime trait
pub trait Runtime: Send + Sync + Clone + 'static {
    /// Spawn a future to run in the background.
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static;

    /// Sleep for the given duration.
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}
