/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Type-state wrappers for LbChannel lifecycle management.
//!
//! Each state is a separate struct, and transitions consume the old state (move semantics).
//! This prevents using a channel in an invalid state at compile time.
//!
//! ```text
//!                +-----------+
//!                |           |
//!                v           |
//! Idle --> Connecting --> Ready <--+--> Ejected
//!                ^                       |
//!                |                       |
//!                +-----------------------+
//! ```
//!
//! State changes are all one-shot. [`ConnectingChannel`] and [`EjectedChannel`] are
//! [`Future`]. The caller (typically a pool) uses [`KeyedFutures`] to
//! manage multiple in-flight state changes and handle cancellation by key.
//!
//! The state types hold the raw service `S` directly. In-flight tracking and
//! load reporting are handled separately by [`LbChannel`] at the pool level.
//!
//! [`KeyedFutures`]: crate::client::loadbalance::keyed_futures::KeyedFutures
//! [`LbChannel`]: crate::client::loadbalance::channel::LbChannel

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use pin_project_lite::pin_project;
use tower::Service;
use tower::load::Load;

use crate::client::endpoint::{Connector, EndpointAddress};
use crate::client::loadbalance::outlier_detection::OutlierStatsRegistry;
use crate::common::async_util::BoxFuture;

// ---------------------------------------------------------------------------
// EndpointCounters / OutlierChannelState
// ---------------------------------------------------------------------------

/// Lock-free success/failure counter for one endpoint. Records RPC
/// outcomes from the data path; the outlier-detection actor reads and
/// resets between intervals.
#[derive(Debug, Default)]
pub(crate) struct EndpointCounters {
    success: AtomicU64,
    failure: AtomicU64,
}

impl EndpointCounters {
    pub(crate) fn record_success(&self) {
        self.success.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_failure(&self) {
        self.failure.fetch_add(1, Ordering::Relaxed);
    }

    /// Read and zero both counters. The two swaps are not atomic against
    /// each other; bias from in-flight RPCs is bounded and well below
    /// the precision of the failure-percentage threshold.
    pub(crate) fn snapshot_and_reset(&self) -> (u64, u64) {
        let s = self.success.swap(0, Ordering::Relaxed);
        let f = self.failure.swap(0, Ordering::Relaxed);
        (s, f)
    }
}

/// Per-channel outlier-detection state, shared via `Arc` between the
/// data path (per-RPC outcome recording + threshold-based ejection)
/// and the housekeeping actor.
///
/// Ejection state is encoded in [`Self::ejected_at_nanos`]: zero means
/// not ejected, non-zero is the nanos-since-epoch of the ejection's
/// start. [`Self::try_eject`] / [`Self::try_uneject`] use CAS so callers
/// can update registry-level counters exactly once per transition.
#[derive(Debug)]
pub(crate) struct OutlierChannelState {
    addr: EndpointAddress,
    counters: EndpointCounters,
    /// Bumped on each ejection; decremented (saturating) on each
    /// healthy interval.
    ejection_multiplier: AtomicU32,
    /// `0` when not ejected; otherwise nanos since [`Self::epoch`] of
    /// the current ejection's start.
    ejected_at_nanos: AtomicU64,
    /// Origin for `ejected_at_nanos`. Set at construction.
    epoch: Instant,
}

impl OutlierChannelState {
    pub(crate) fn new(addr: EndpointAddress) -> Self {
        Self {
            addr,
            counters: EndpointCounters::default(),
            ejection_multiplier: AtomicU32::new(0),
            ejected_at_nanos: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// Endpoint address this state belongs to.
    pub(crate) fn addr(&self) -> &EndpointAddress {
        &self.addr
    }

    pub(crate) fn record_success(&self) {
        self.counters.record_success();
    }

    pub(crate) fn record_failure(&self) {
        self.counters.record_failure();
    }

    /// Per-RPC entry point. Bumps the success or failure counter
    /// depending on the RPC's outcome. Ejection decisions are deferred
    /// to the next sweep (gRFC A50 §6).
    pub(crate) fn record_outcome(&self, success: bool) {
        if success {
            self.record_success();
        } else {
            self.record_failure();
        }
    }

    /// Returns `(success, failure)` without resetting. The two reads
    /// are not atomic together; bias is bounded by in-flight RPCs.
    pub(crate) fn counters(&self) -> (u64, u64) {
        let s = self.counters.success.load(Ordering::Relaxed);
        let f = self.counters.failure.load(Ordering::Relaxed);
        (s, f)
    }

    /// Read and zero the counters. Returns `(success, failure)`.
    pub(crate) fn snapshot_and_reset(&self) -> (u64, u64) {
        self.counters.snapshot_and_reset()
    }

    /// Atomically mark this channel as ejected starting at `now`.
    /// Returns `true` on the not-ejected → ejected transition and
    /// bumps the multiplier; `false` if already ejected.
    pub(crate) fn try_eject(&self, now: Instant) -> bool {
        let nanos = now
            .saturating_duration_since(self.epoch)
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        // 0 means "not ejected"; use 1 as a sentinel if the channel
        // was created at exactly `now`.
        let stamp = nanos.max(1);
        if self
            .ejected_at_nanos
            .compare_exchange(0, stamp, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        self.ejection_multiplier.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Atomically clear the ejection. Returns `true` on the
    /// ejected → not-ejected transition.
    pub(crate) fn try_uneject(&self) -> bool {
        self.ejected_at_nanos.swap(0, Ordering::AcqRel) != 0
    }

    /// Current ejection state.
    pub(crate) fn is_ejected(&self) -> bool {
        self.ejected_at_nanos.load(Ordering::Acquire) != 0
    }

    /// Returns the elapsed time since this channel was ejected, or
    /// `None` if it is not currently ejected.
    pub(crate) fn ejected_duration(&self, now: Instant) -> Option<Duration> {
        let nanos = self.ejected_at_nanos.load(Ordering::Relaxed);
        if nanos == 0 {
            return None;
        }
        let ejected_at = self.epoch + Duration::from_nanos(nanos);
        Some(now.saturating_duration_since(ejected_at))
    }

    /// Current ejection multiplier.
    pub(crate) fn ejection_multiplier(&self) -> u32 {
        self.ejection_multiplier.load(Ordering::Relaxed)
    }

    /// Decrement the multiplier, saturating at zero. Atomic against
    /// concurrent `try_eject` and other decrements.
    pub(crate) fn decrement_multiplier(&self) {
        // TODO: Switch to `try_update` and remove this allow once MSRV >= 1.95.
        #[allow(deprecated)]
        let _ = self
            .ejection_multiplier
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v > 0 { Some(v - 1) } else { None }
            });
    }

    /// Test-only multiplier setter for driving housekeeping without
    /// going through `try_eject`.
    #[cfg(test)]
    pub(crate) fn set_ejection_multiplier(&self, value: u32) {
        self.ejection_multiplier.store(value, Ordering::Relaxed);
    }
}

/// Configuration for an ejected channel.
#[derive(Debug, Clone)]
pub(crate) struct EjectionConfig {
    /// How long the channel is ejected before it can return to service.
    pub timeout: Duration,
    /// Whether the channel needs a fresh connection after ejection expires (e.g. after consecutive timeouts).
    pub needs_reconnect: bool,
}

/// Result of an ejection expiring.
pub(crate) enum UnejectedChannel<S> {
    /// Cooldown elapsed; the original connection is reused with its
    /// outlier state reattached.
    Ready(ReadyChannel<S>),
    /// A fresh connection has been started.
    Connecting(ConnectingChannel<S>),
}

// ---------------------------------------------------------------------------
// IdleChannel
// ---------------------------------------------------------------------------

/// An idle channel that only stores an address. It is the entry point for
/// starting a connection attempt.
pub(crate) struct IdleChannel {
    addr: EndpointAddress,
}

impl IdleChannel {
    pub(crate) fn new(addr: EndpointAddress) -> Self {
        Self { addr }
    }

    /// Start connecting to the endpoint. Consumes the idle channel.
    /// The resolved [`ReadyChannel`] will carry the
    /// `Arc<OutlierChannelState>` from `registry.add_channel(addr)` —
    /// idempotent, so a re-discovered or reconnected address keeps
    /// its existing counters and ejection state.
    pub(crate) fn connect<C: Connector>(
        self,
        connector: Arc<C>,
        registry: Arc<OutlierStatsRegistry>,
    ) -> ConnectingChannel<C::Service>
    where
        C::Service: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr, registry)
    }
}

// ---------------------------------------------------------------------------
// ConnectingChannel
// ---------------------------------------------------------------------------

/// A channel that is in the process of connecting.
///
/// `impl Future<Output = ReadyChannel<S>>` — the connector's
/// service-future is wrapped at construction time into an async
/// block that looks up the per-channel outlier state from `registry`
/// (via `add_channel`) and produces a fully-formed `ReadyChannel`.
/// Cancellation is handled externally via [`KeyedFutures::cancel`].
///
/// [`KeyedFutures::cancel`]: crate::client::loadbalance::keyed_futures::KeyedFutures::cancel
pub(crate) struct ConnectingChannel<S> {
    inner: Pin<Box<dyn Future<Output = ReadyChannel<S>> + Send>>,
}

impl<S: Send + 'static> ConnectingChannel<S> {
    pub(crate) fn new(
        fut: BoxFuture<S>,
        addr: EndpointAddress,
        registry: Arc<OutlierStatsRegistry>,
    ) -> Self {
        Self {
            inner: Box::pin(async move {
                let svc = fut.await;
                let outlier = registry.add_channel(addr.clone());
                ReadyChannel::new(addr, svc, outlier)
            }),
        }
    }
}

impl<S: Send + 'static> Future for ConnectingChannel<S> {
    type Output = ReadyChannel<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().poll(cx)
    }
}

// ---------------------------------------------------------------------------
// ReadyChannel
// ---------------------------------------------------------------------------

/// A channel that is connected and ready to serve requests.
///
/// Holds the raw service `S` and delegates [`Service`] calls directly,
/// preserving `S::Future` and `S::Error`. Shares
/// [`OutlierChannelState`] with the outlier-detection actor via `Arc`.
#[derive(Clone)]
pub(crate) struct ReadyChannel<S> {
    addr: EndpointAddress,
    inner: S,
    outlier: Arc<OutlierChannelState>,
}

impl<S> ReadyChannel<S> {
    pub(crate) fn new(addr: EndpointAddress, inner: S, outlier: Arc<OutlierChannelState>) -> Self {
        Self {
            addr,
            inner,
            outlier,
        }
    }

    /// Per-channel outlier-detection state. Cloned cheaply via `Arc`.
    pub(crate) fn outlier(&self) -> &Arc<OutlierChannelState> {
        &self.outlier
    }

    /// Record the outcome of an RPC handled by this channel on the
    /// attached outlier-detection state. The data path calls this on
    /// every completed RPC; ejection decisions happen at the next
    /// sweep (gRFC A50 §6).
    pub(crate) fn record_outcome(&self, success: bool) {
        self.outlier.record_outcome(success);
    }

    /// Eject this channel. Consumes self; the outlier state is moved
    /// into the [`EjectedChannel`] so it can be reattached to the
    /// [`ReadyChannel`] produced when the cooldown elapses.
    pub(crate) fn eject<C>(
        self,
        config: EjectionConfig,
        connector: Arc<C>,
        registry: Arc<OutlierStatsRegistry>,
    ) -> EjectedChannel<S>
    where
        C: Connector<Service = S> + Send + Sync + 'static,
    {
        let ejection_timer = tokio::time::sleep(config.timeout);
        EjectedChannel {
            addr: self.addr,
            inner: self.inner,
            outlier: self.outlier,
            registry,
            config,
            connector,
            ejection_timer,
        }
    }

    /// Drop the connection and start a fresh connect for the same
    /// address. The outlier state is re-attached from `registry`
    /// when the new connect resolves.
    pub(crate) fn reconnect<C: Connector<Service = S>>(
        self,
        connector: Arc<C>,
        registry: Arc<OutlierStatsRegistry>,
    ) -> ConnectingChannel<S>
    where
        S: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr, registry)
    }
}

impl<S, Req> Service<Req> for ReadyChannel<S>
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self.inner.call(req)
    }
}

impl<S: Load> Load for ReadyChannel<S> {
    type Metric = S::Metric;

    fn load(&self) -> Self::Metric {
        self.inner.load()
    }
}

// ---------------------------------------------------------------------------
// EjectedChannel
// ---------------------------------------------------------------------------

pin_project! {
    /// A channel that has been ejected and is cooling down. The
    /// underlying connection is kept alive but cannot serve requests.
    ///
    /// `impl Future<Output = UnejectedChannel<S>>` — resolves when
    /// `config.timeout` elapses, to [`UnejectedChannel::Ready`] if
    /// `needs_reconnect` is false, otherwise
    /// [`UnejectedChannel::Connecting`].
    pub(crate) struct EjectedChannel<S> {
        addr: EndpointAddress,
        inner: S,
        outlier: Arc<OutlierChannelState>,
        registry: Arc<OutlierStatsRegistry>,
        config: EjectionConfig,
        connector: Arc<dyn Connector<Service = S> + Send + Sync>,
        #[pin]
        ejection_timer: tokio::time::Sleep,
    }
}

impl<S: Clone + Send + 'static> Future for EjectedChannel<S> {
    type Output = UnejectedChannel<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.ejection_timer.poll(cx) {
            Poll::Ready(()) => {
                if this.config.needs_reconnect {
                    let fut = this.connector.connect(this.addr);
                    let connecting =
                        ConnectingChannel::new(fut, this.addr.clone(), this.registry.clone());
                    Poll::Ready(UnejectedChannel::Connecting(connecting))
                } else {
                    let ready = ReadyChannel::new(
                        this.addr.clone(),
                        this.inner.clone(),
                        this.outlier.clone(),
                    );
                    Poll::Ready(UnejectedChannel::Ready(ready))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::loadbalance::keyed_futures::KeyedFutures;
    use futures_util::task::noop_waker;
    use std::future;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone, Debug)]
    struct MockService;

    impl Service<&'static str> for MockService {
        type Response = &'static str;
        type Error = &'static str;
        type Future = future::Ready<Result<&'static str, &'static str>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: &'static str) -> Self::Future {
            future::ready(Ok("ok"))
        }
    }

    struct MockConnector {
        connect_count: Arc<AtomicU32>,
    }

    impl MockConnector {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                connect_count: Arc::new(AtomicU32::new(0)),
            })
        }
    }

    impl Connector for MockConnector {
        type Service = MockService;

        fn connect(&self, _addr: &EndpointAddress) -> BoxFuture<Self::Service> {
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(future::ready(MockService))
        }
    }

    fn test_addr() -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", 8080)
    }

    fn noop_cx() -> Context<'static> {
        Context::from_waker(Box::leak(Box::new(noop_waker())))
    }

    /// Throwaway registry for tests that don't observe ejection
    /// state — defaults to the disabled config, so the housekeeping
    /// actor would never run anyway.
    fn test_registry() -> Arc<OutlierStatsRegistry> {
        use crate::xds::resource::outlier_detection::OutlierDetectionConfig;
        use arc_swap::ArcSwap;
        OutlierStatsRegistry::new(Arc::new(ArcSwap::from_pointee(
            OutlierDetectionConfig::default(),
        )))
        .0
    }

    #[tokio::test]
    async fn test_idle_to_connecting() {
        let connector = MockConnector::new();
        let _connecting = IdleChannel::new(test_addr()).connect(connector.clone(), test_registry());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_connecting_future_yields_ready_channel() {
        let connector = MockConnector::new();
        let ready: ReadyChannel<MockService> = IdleChannel::new(test_addr())
            .connect(connector, test_registry())
            .await;
        assert_eq!(ready.outlier().addr(), &test_addr());
    }

    #[tokio::test]
    async fn test_ready_service_delegates() {
        let connector = MockConnector::new();
        let mut ready = IdleChannel::new(test_addr())
            .connect(connector, test_registry())
            .await;
        let resp: &str = ready.call("hello").await.unwrap();
        assert_eq!(resp, "ok");
    }

    #[tokio::test]
    async fn test_ready_to_connecting_via_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone(), test_registry())
            .await;
        let _reconnecting = ready.reconnect(connector.clone(), test_registry());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }

    // --- KeyedFutures integration ---

    #[tokio::test]
    async fn test_connecting_in_keyed_futures() {
        let (tx, rx) = tokio::sync::oneshot::channel::<MockService>();
        let connecting = ConnectingChannel::new(
            Box::pin(async move { rx.await.unwrap() }),
            test_addr(),
            test_registry(),
        );

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> = KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        tx.send(MockService).unwrap();

        match set.poll_next(&mut noop_cx()) {
            Poll::Ready(Some((addr, _))) => assert_eq!(addr, test_addr()),
            _ => panic!("expected Ready"),
        }
    }

    #[tokio::test]
    async fn test_connecting_cancelled_via_keyed_futures() {
        let connecting = ConnectingChannel::new(
            Box::pin(future::pending::<MockService>()),
            test_addr(),
            test_registry(),
        );

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> = KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        set.cancel(&test_addr()).unwrap();
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Ready(None)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_ready() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone(), test_registry())
            .await;
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: false,
            },
            connector,
            test_registry(),
        );

        let mut set: KeyedFutures<EndpointAddress, UnejectedChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), ejected).unwrap();

        let (addr, result) = futures_util::future::poll_fn(|cx| set.poll_next(cx))
            .await
            .unwrap();
        assert_eq!(addr, test_addr());
        assert!(matches!(result, UnejectedChannel::Ready(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_needs_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone(), test_registry())
            .await;
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: true,
            },
            connector.clone(),
            test_registry(),
        );

        let mut set: KeyedFutures<EndpointAddress, UnejectedChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), ejected).unwrap();

        let (addr, result) = futures_util::future::poll_fn(|cx| set.poll_next(cx))
            .await
            .unwrap();
        assert_eq!(addr, test_addr());
        assert!(matches!(result, UnejectedChannel::Connecting(_)));
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }
}
