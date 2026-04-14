//! [`KeyedFutures`]: a cancellable, keyed set of futures.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::stream::FuturesUnordered;
use tokio_util::sync::CancellationToken;

use crate::common::async_util::BoxFuture;

/// Errors returned by [`KeyedFutures`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum KeyedFuturesError<K: std::fmt::Debug> {
    /// A future for this key is already running.
    #[error("key {0:?} already exists")]
    DuplicateKey(K),
    /// No future is running for the given key.
    #[error("key {0:?} not found")]
    KeyNotFound(K),
}

/// A cancellable, keyed set of futures.
///
/// Each future is associated with a key `K` and produces a value `T`.
/// Futures can be cancelled individually by key. [`poll_next`] drives all
/// futures concurrently and yields `(K, T)` when one completes; cancelled
/// futures are silently skipped.
///
/// Intended for use inside [`tower::Service::poll_ready`] to manage large number of
/// concurrent, cancellable operations (e.g. pending connection attempts).
pub(crate) struct KeyedFutures<K, T> {
    cancellations: HashMap<K, CancellationToken>,
    futures: FuturesUnordered<BoxFuture<(K, Option<T>)>>,
}

impl<K, T> KeyedFutures<K, T>
where
    K: Hash + Eq + Clone + Send + std::fmt::Debug + 'static,
    T: Send + 'static,
{
    pub(crate) fn new() -> Self {
        Self {
            cancellations: HashMap::new(),
            futures: FuturesUnordered::new(),
        }
    }

    /// Add a future keyed by `key`. Returns `Err(DuplicateKey)` if a future
    /// for this key is already running.
    pub(crate) fn add<F>(&mut self, key: K, fut: F) -> Result<(), KeyedFuturesError<K>>
    where
        F: Future<Output = T> + Send + 'static,
    {
        if self.cancellations.contains_key(&key) {
            return Err(KeyedFuturesError::DuplicateKey(key));
        }
        let token = CancellationToken::new();
        self.cancellations.insert(key.clone(), token.clone());

        self.futures.push(Box::pin(async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => (key, None),
                t = fut => (key, Some(t)),
            }
        }));
        Ok(())
    }

    /// Cancel the future for `key`. Returns `Err(KeyNotFound)` if no future
    /// is running for the given key.
    pub(crate) fn cancel(&mut self, key: &K) -> Result<(), KeyedFuturesError<K>> {
        match self.cancellations.remove(key) {
            Some(token) => {
                token.cancel();
                Ok(())
            }
            None => Err(KeyedFuturesError::KeyNotFound(key.clone())),
        }
    }

    /// Returns the number of futures currently running (including cancelled
    /// ones not yet polled to completion).
    pub(crate) fn len(&self) -> usize {
        self.futures.len()
    }

    /// Advance the internal futures. Yields `(K, T)` when a future completes,
    /// skipping cancelled futures silently.
    ///
    /// Returns:
    /// - `Poll::Ready(Some((key, output)))` — a future completed successfully.
    /// - `Poll::Pending` — no futures ready yet; the waker will be notified.
    /// - `Poll::Ready(None)` — all futures have completed or been cancelled.
    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<(K, T)>> {
        loop {
            match Pin::new(&mut self.futures).poll_next(cx) {
                Poll::Ready(Some((key, Some(output)))) => {
                    self.cancellations.remove(&key);
                    return Poll::Ready(Some((key, output)));
                }
                Poll::Ready(Some((_, None))) => continue, // skip cancelled futures
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::task::noop_waker;

    fn noop_cx() -> Context<'static> {
        // SAFETY: the waker is never dereferenced; used only to satisfy the
        // Context API. FuturesUnordered manages internal task wakeups
        // independently of this outer waker.
        Context::from_waker(Box::leak(Box::new(noop_waker())))
    }

    #[tokio::test]
    async fn test_add_and_poll() {
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        set.add("a", async { 1 }).unwrap();
        set.add("b", async { 2 }).unwrap();

        let mut results = vec![];
        while let Poll::Ready(Some(item)) = set.poll_next(&mut noop_cx()) {
            results.push(item);
        }
        results.sort();
        assert_eq!(results, vec![("a", 1), ("b", 2)]);
    }

    #[tokio::test]
    async fn test_poll_pending_then_ready() {
        // Use a oneshot channel so the future is pending until we send.
        // FuturesUnordered's internal TaskWaker is woken by tx.send(),
        // so the next poll_next sees the result without needing yield_now().
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
        set.add("a", async move { rx.await.unwrap() }).unwrap();

        // Before send: pending.
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        // Signal the future to complete.
        tx.send(42).unwrap();

        // FuturesUnordered's internal waker was notified; next poll sees result.
        assert_eq!(set.poll_next(&mut noop_cx()), Poll::Ready(Some(("a", 42))));
    }

    #[tokio::test]
    async fn test_duplicate_key_rejected() {
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        set.add("a", async { 1 }).unwrap();
        assert!(matches!(
            set.add("a", async { 2 }),
            Err(KeyedFuturesError::DuplicateKey("a"))
        ));
    }

    #[tokio::test]
    async fn test_cancel_skipped_in_poll() {
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        let (tx_a, rx_a) = tokio::sync::oneshot::channel::<u32>();
        let (tx_b, rx_b) = tokio::sync::oneshot::channel::<u32>();

        set.add("a", async move { rx_a.await.unwrap() }).unwrap();
        set.add("b", async move { rx_b.await.unwrap() }).unwrap();

        // Both pending.
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        // Cancel "a", complete "b".
        set.cancel(&"a").unwrap();
        tx_b.send(42).unwrap();
        drop(tx_a);

        // "a" is silently skipped; only "b" is yielded.
        assert_eq!(set.poll_next(&mut noop_cx()), Poll::Ready(Some(("b", 42))));
        assert_eq!(set.poll_next(&mut noop_cx()), Poll::Ready(None));
    }

    #[tokio::test]
    async fn test_cancel_nonexistent_returns_error() {
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        assert!(matches!(
            set.cancel(&"missing"),
            Err(KeyedFuturesError::KeyNotFound("missing"))
        ));
    }

    #[tokio::test]
    async fn test_reuse_key_after_completion() {
        let mut set: KeyedFutures<&str, u32> = KeyedFutures::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
        set.add("a", async move { rx.await.unwrap() }).unwrap();

        tx.send(1).unwrap();
        assert_eq!(set.poll_next(&mut noop_cx()), Poll::Ready(Some(("a", 1))));

        // Key is free after completion — can be re-added.
        set.add("a", async { 2 }).unwrap();
        assert_eq!(set.poll_next(&mut noop_cx()), Poll::Ready(Some(("a", 2))));
    }
}
