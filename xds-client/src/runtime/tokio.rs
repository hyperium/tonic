//! `tokio` based runtime implementation.

use crate::runtime::Runtime;
use std::future::Future;
use std::time::Duration;

/// Tokio-based runtime implementation.
#[derive(Clone, Debug, Default)]
pub struct TokioRuntime;

impl Runtime for TokioRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
