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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use pin_project_lite::pin_project;
use tower::Service;
use tower::load::Load;

use crate::client::endpoint::{Connector, EndpointAddress};
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
    /// each other — RPCs landing between them may bias the snapshot by
    /// a small number of events, well below the precision of the
    /// failure-percentage threshold.
    pub(crate) fn snapshot_and_reset(&self) -> (u64, u64) {
        let s = self.success.swap(0, Ordering::Relaxed);
        let f = self.failure.swap(0, Ordering::Relaxed);
        (s, f)
    }
}

/// Per-channel outlier-detection state, shared (via `Arc`) between
/// the data path (per-RPC outcome recording + threshold-based ejection),
/// the outlier-detection actor (interval-based housekeeping), and the
/// load balancer (consults `is_ejected` / `ejected_duration` on
/// reconnect).
///
/// All fields are atomics so the data path can mutate them without
/// locking. Ejection state is encoded in [`Self::ejected_at_nanos`]:
/// zero means not ejected, non-zero is the nanos-since-epoch of the
/// ejection's start. [`Self::try_eject`] / [`Self::try_uneject`] use
/// CAS to flip the field atomically and report whether the transition
/// fired (so callers can update registry-level counters exactly once
/// per transition).
#[derive(Debug)]
pub(crate) struct OutlierChannelState {
    counters: EndpointCounters,
    /// Whether this channel currently contributes to the registry's
    /// `qualifying_count`. Set when `total` first reaches
    /// `request_volume` in the current interval; cleared on counter
    /// reset.
    is_qualifying: AtomicBool,
    /// Number of times this channel has been ejected. Bumped on each
    /// ejection; decremented (saturating) on each healthy interval.
    ejection_multiplier: AtomicU32,
    /// `0` when not ejected. Otherwise nanos since [`Self::epoch`] of
    /// the current ejection's start. Single source of truth for
    /// "is this channel ejected right now?".
    ejected_at_nanos: AtomicU64,
    /// Reference instant used as the origin for `ejected_at_nanos`.
    /// Established at construction and never changes.
    epoch: Instant,
}

impl Default for OutlierChannelState {
    fn default() -> Self {
        Self::new()
    }
}

impl OutlierChannelState {
    pub(crate) fn new() -> Self {
        Self {
            counters: EndpointCounters::default(),
            is_qualifying: AtomicBool::new(false),
            ejection_multiplier: AtomicU32::new(0),
            ejected_at_nanos: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    pub(crate) fn record_success(&self) {
        self.counters.record_success();
    }

    pub(crate) fn record_failure(&self) {
        self.counters.record_failure();
    }

    /// Read the current counter values without resetting. Returns
    /// `(success, failure)`. The two reads are not atomic against
    /// each other but the difference is bounded by concurrent in-flight
    /// RPCs and is below the precision of the failure-percentage check.
    pub(crate) fn counters(&self) -> (u64, u64) {
        let s = self.counters.success.load(Ordering::Relaxed);
        let f = self.counters.failure.load(Ordering::Relaxed);
        (s, f)
    }

    /// Read and zero the counters. Returns `(success, failure)`.
    pub(crate) fn snapshot_and_reset(&self) -> (u64, u64) {
        self.counters.snapshot_and_reset()
    }

    /// Try to set `is_qualifying` to `true`. Returns `true` if this
    /// call performed the false → true transition, so callers can
    /// increment a registry-level counter exactly once per crossing.
    pub(crate) fn mark_qualifying(&self) -> bool {
        !self.is_qualifying.swap(true, Ordering::AcqRel)
    }

    /// Clear `is_qualifying`. Returns the previous value.
    pub(crate) fn clear_qualifying(&self) -> bool {
        self.is_qualifying.swap(false, Ordering::AcqRel)
    }

    /// Atomically mark this channel as ejected starting at `now`.
    /// Returns `true` if this call performed the not-ejected →
    /// ejected transition (so callers can update registry-level
    /// counters exactly once per ejection). Bumps the multiplier on
    /// transition.
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

    /// Atomically clear the ejection. Returns `true` if this call
    /// performed the ejected → not-ejected transition.
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

    /// Decrement the multiplier saturating at zero. Called by the
    /// actor on healthy intervals.
    pub(crate) fn decrement_multiplier(&self) {
        let prev = self.ejection_multiplier.load(Ordering::Relaxed);
        if prev > 0 {
            self.ejection_multiplier.store(prev - 1, Ordering::Relaxed);
        }
    }

    /// Test-only setter for the ejection multiplier; lets tests drive
    /// housekeeping behavior without going through `try_eject`.
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
    /// The channel is ready to serve again (ejection expired, no
    /// reconnect needed). The consumer wraps the bare service into a
    /// [`ReadyChannel`] using the registry-supplied
    /// [`OutlierChannelState`].
    Ready(S),
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
    pub(crate) fn connect<C: Connector>(self, connector: Arc<C>) -> ConnectingChannel<C::Service>
    where
        C::Service: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr)
    }
}

// ---------------------------------------------------------------------------
// ConnectingChannel
// ---------------------------------------------------------------------------

/// A channel that is in the process of connecting.
///
/// Implements [`Future`] -- resolves to the connected service `S`
/// when the connection completes. The consumer wraps that into a
/// [`ReadyChannel`] (attaching its [`OutlierChannelState`]).
/// Cancellation is handled externally via [`KeyedFutures::cancel`].
///
/// `ConnectingChannel` deliberately does not carry an
/// [`OutlierChannelState`]: it does not serve traffic, so it has
/// nothing to count or signal.
///
/// [`KeyedFutures::cancel`]: crate::client::loadbalance::keyed_futures::KeyedFutures::cancel
pub(crate) struct ConnectingChannel<S> {
    inner: Pin<Box<dyn Future<Output = S> + Send>>,
}

impl<S: Send + 'static> ConnectingChannel<S> {
    /// Start a connection. The address is kept by the caller (it is
    /// typically the key in a `KeyedFutures` map); only the future is
    /// stored here.
    pub(crate) fn new(fut: BoxFuture<S>, _addr: EndpointAddress) -> Self {
        Self { inner: fut }
    }
}

impl<S: Send + 'static> Future for ConnectingChannel<S> {
    type Output = S;

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
/// preserving `S::Future` and `S::Error` with no wrapping or type
/// erasure. The `Arc<OutlierChannelState>` is shared with the outlier-
/// detection actor for stats accumulation and edge-triggered ejection;
/// because only `ReadyChannel` serves traffic, only `ReadyChannel`
/// carries this state.
#[derive(Clone)]
pub(crate) struct ReadyChannel<S> {
    addr: EndpointAddress,
    inner: S,
    outlier: Arc<OutlierChannelState>,
}

impl<S> ReadyChannel<S> {
    /// Wrap a connected service `S` into a [`ReadyChannel`] using the
    /// caller-supplied outlier state.
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

    /// Endpoint address this channel was created for.
    pub(crate) fn addr(&self) -> &EndpointAddress {
        &self.addr
    }

    /// Eject this channel (e.g., due to outlier detection). Consumes
    /// self. The outlier state remains in the registry; only the
    /// service and address are passed into [`EjectedChannel`] (which
    /// just times the cooldown).
    pub(crate) fn eject<C>(self, config: EjectionConfig, connector: Arc<C>) -> EjectedChannel<S>
    where
        C: Connector<Service = S> + Send + Sync + 'static,
    {
        let ejection_timer = tokio::time::sleep(config.timeout);
        EjectedChannel {
            addr: self.addr,
            inner: self.inner,
            config,
            connector,
            ejection_timer,
        }
    }

    /// Start reconnecting. Consumes self, dropping the old connection.
    /// The outlier state remains in the registry; the consumer
    /// re-attaches it when the new [`ReadyChannel`] is constructed.
    pub(crate) fn reconnect<C: Connector<Service = S>>(
        self,
        connector: Arc<C>,
    ) -> ConnectingChannel<S>
    where
        S: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr)
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
    /// A channel that has been ejected and is cooling down.
    ///
    /// The underlying connection is kept alive but cannot serve
    /// requests. Implements [`Future`] -- resolves once the ejection
    /// timer expires to either:
    /// - [`UnejectedChannel::Ready`] if no reconnect is needed
    /// - [`UnejectedChannel::Connecting`] if a fresh connection is required
    ///
    /// `EjectedChannel` deliberately does not carry an
    /// [`OutlierChannelState`]: the state lives in the registry, keyed
    /// by address, and the consumer re-attaches it when the channel
    /// transitions back to [`ReadyChannel`].
    pub(crate) struct EjectedChannel<S> {
        addr: EndpointAddress,
        inner: S,
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
                    Poll::Ready(UnejectedChannel::Connecting(ConnectingChannel::new(
                        fut,
                        this.addr.clone(),
                    )))
                } else {
                    Poll::Ready(UnejectedChannel::Ready(this.inner.clone()))
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

    #[tokio::test]
    async fn test_idle_to_connecting() {
        let connector = MockConnector::new();
        let _connecting = IdleChannel::new(test_addr()).connect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 1);
    }

    fn wrap_ready(addr: EndpointAddress, svc: MockService) -> ReadyChannel<MockService> {
        ReadyChannel::new(addr, svc, Arc::new(OutlierChannelState::new()))
    }

    #[tokio::test]
    async fn test_connecting_future_yields_service() {
        let connector = MockConnector::new();
        let svc: MockService = IdleChannel::new(test_addr()).connect(connector).await;
        // The bare service is what `ConnectingChannel` resolves to.
        let _ready = wrap_ready(test_addr(), svc);
    }

    #[tokio::test]
    async fn test_ready_service_delegates() {
        let connector = MockConnector::new();
        let svc = IdleChannel::new(test_addr()).connect(connector).await;
        let mut ready = wrap_ready(test_addr(), svc);
        let resp: &str = ready.call("hello").await.unwrap();
        assert_eq!(resp, "ok");
    }

    #[tokio::test]
    async fn test_ready_to_connecting_via_reconnect() {
        let connector = MockConnector::new();
        let svc = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let ready = wrap_ready(test_addr(), svc);
        let _reconnecting = ready.reconnect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }

    // --- KeyedFutures integration ---

    #[tokio::test]
    async fn test_connecting_in_keyed_futures() {
        let (tx, rx) = tokio::sync::oneshot::channel::<MockService>();
        let connecting =
            ConnectingChannel::new(Box::pin(async move { rx.await.unwrap() }), test_addr());

        let mut set: KeyedFutures<EndpointAddress, MockService> = KeyedFutures::new();
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
        let connecting =
            ConnectingChannel::new(Box::pin(future::pending::<MockService>()), test_addr());

        let mut set: KeyedFutures<EndpointAddress, MockService> = KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        set.cancel(&test_addr()).unwrap();
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Ready(None)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_ready() {
        let connector = MockConnector::new();
        let svc = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let ready = wrap_ready(test_addr(), svc);
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: false,
            },
            connector,
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
        let svc = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let ready = wrap_ready(test_addr(), svc);
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: true,
            },
            connector.clone(),
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
