//! LbChannel: an instrumented channel wrapper with in-flight request tracking.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tower::load::Load;
use tower::{BoxError, Service};

use crate::client::endpoint::EndpointAddress;
use crate::common::async_util::BoxFuture;

/// RAII guard that increments an in-flight counter on creation and decrements on drop.
/// Ensures accurate tracking even when futures are cancelled.
struct InFlightGuard {
    counter: Arc<AtomicU64>,
}

impl InFlightGuard {
    fn acquire(counter: Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A channel wrapper that tracks in-flight requests for load balancing.
///
/// `LbChannel` wraps an inner service `S` and maintains an atomic counter of
/// in-flight requests. This counter is used by P2C load balancers (via the
/// [`Load`] trait) to prefer endpoints with fewer active requests.
///
/// All clones of an `LbChannel` share the same in-flight counter.
pub(crate) struct LbChannel<S> {
    addr: EndpointAddress,
    inner: S,
    in_flight: Arc<AtomicU64>,
}

impl<S> LbChannel<S> {
    /// Create a new `LbChannel` wrapping the given service.
    pub(crate) fn new(addr: EndpointAddress, inner: S) -> Self {
        Self {
            addr,
            inner,
            in_flight: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns the endpoint address.
    pub(crate) fn addr(&self) -> &EndpointAddress {
        &self.addr
    }

    /// Returns the current number of in-flight requests.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }
}

impl<S: Clone> Clone for LbChannel<S> {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr.clone(),
            inner: self.inner.clone(),
            in_flight: self.in_flight.clone(),
        }
    }
}

impl<S, Req> Service<Req> for LbChannel<S>
where
    S: Service<Req> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<Result<S::Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let mut inner = self.inner.clone();
        let guard = InFlightGuard::acquire(self.in_flight.clone());

        Box::pin(async move {
            let _guard = guard;
            inner.call(req).await.map_err(Into::into)
        })
    }
}

impl<S> Load for LbChannel<S> {
    type Metric = u64;

    fn load(&self) -> Self::Metric {
        self.in_flight.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future;
    use std::task::Poll;

    fn test_addr() -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", 8080)
    }

    #[derive(Clone)]
    struct MockService;

    impl Service<&'static str> for MockService {
        type Response = &'static str;
        type Error = BoxError;
        type Future = future::Ready<Result<&'static str, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: &'static str) -> Self::Future {
            future::ready(Ok("ok"))
        }
    }

    #[tokio::test]
    async fn test_in_flight_increments_and_decrements() {
        let mut ch = LbChannel::new(test_addr(), MockService);
        assert_eq!(ch.in_flight(), 0);

        let fut = ch.call("hello");
        assert_eq!(ch.in_flight(), 1);

        let resp = fut.await.unwrap();
        assert_eq!(resp, "ok");
        assert_eq!(ch.in_flight(), 0);
    }

    #[tokio::test]
    async fn test_in_flight_on_future_drop() {
        let mut ch = LbChannel::new(test_addr(), MockService);
        let fut = ch.call("hello");
        assert_eq!(ch.in_flight(), 1);

        drop(fut);
        assert_eq!(ch.in_flight(), 0);
    }

    #[tokio::test]
    async fn test_clone_shares_in_flight() {
        let mut ch1 = LbChannel::new(test_addr(), MockService);
        let ch2 = ch1.clone();

        let fut = ch1.call("hello");
        assert_eq!(ch1.in_flight(), 1);
        assert_eq!(ch2.in_flight(), 1);

        let _ = fut.await;
        assert_eq!(ch1.in_flight(), 0);
        assert_eq!(ch2.in_flight(), 0);
    }

    #[test]
    fn test_load_returns_in_flight() {
        let ch = LbChannel::new(test_addr(), MockService);
        assert_eq!(Load::load(&ch), 0);

        ch.in_flight.fetch_add(3, Ordering::Relaxed);
        assert_eq!(Load::load(&ch), 3);
    }
}
