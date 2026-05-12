//! [gRFC A50] outlier detection.
//!
//! Work is split across three sites:
//!
//! - **Data path** ([`OutlierStatsRegistry::record_outcome`]): runs
//!   inline per RPC. Updates per-channel counters, applies the
//!   failure-percentage gate, and on transition to ejected sends the
//!   address through an mpsc channel.
//! - **Load balancer**: drains the eject mpsc in `poll_ready`,
//!   consumes the matching [`ReadyChannel`] via
//!   [`ReadyChannel::eject`], and tracks the resulting
//!   [`EjectedChannel`] in a `KeyedFutures`. Each ejected channel's
//!   sleep fires at `base × multiplier` (capped by
//!   `max_ejection_time`); the LB then routes the resolved
//!   [`UnejectedChannel`] back into the ready set.
//! - **Housekeeping actor** ([`spawn_actor`]): on each
//!   `config.interval` tick, resets counters and decrements
//!   multipliers for non-ejected channels. The actor never ejects or
//!   un-ejects.
//!
//! Only the failure-percentage algorithm is implemented; success-rate
//! (cross-endpoint mean/stdev) is left to a follow-up.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md
//! [`ReadyChannel`]: crate::client::loadbalance::channel_state::ReadyChannel
//! [`ReadyChannel::eject`]: crate::client::loadbalance::channel_state::ReadyChannel::eject
//! [`EjectedChannel`]: crate::client::loadbalance::channel_state::EjectedChannel
//! [`UnejectedChannel`]: crate::client::loadbalance::channel_state::UnejectedChannel

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::channel_state::OutlierChannelState;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::OutlierDetectionConfig;

/// Returned when an [`OutlierStatsRegistry`] is handed to a second
/// load balancer. The eject-signal receiver is one-shot.
#[derive(Debug, thiserror::Error)]
#[error("OutlierStatsRegistry is already wired to a LoadBalancer")]
pub(crate) struct RegistryAlreadyWired;

/// Probability source for `enforcing_*` rolls.
pub(crate) trait Rng: Send + Sync + 'static {
    /// Return a uniform random `u32` in `0..100`.
    fn pct_roll(&self) -> u32;
}

/// Default RNG backed by `fastrand`.
struct FastRandRng;

impl Rng for FastRandRng {
    fn pct_roll(&self) -> u32 {
        fastrand::u32(0..100)
    }
}

/// Shared outlier-detection state, owned by `Arc` and accessed
/// concurrently by the data path ([`Self::record_outcome`]), the
/// housekeeping actor ([`Self::run_housekeeping`]), and the load
/// balancer ([`Self::note_uneject`], [`Self::remaining_ejection`]).
pub(crate) struct OutlierStatsRegistry {
    channels: DashMap<EndpointAddress, Arc<OutlierChannelState>>,
    /// Channels with `total >= request_volume` in the active
    /// interval. Drives the `minimum_hosts` gate.
    qualifying_count: AtomicU64,
    /// Channels currently ejected. Drives the
    /// `max_ejection_percent` cap.
    ejected_count: AtomicU64,
    config: OutlierDetectionConfig,
    rng: Box<dyn Rng>,
    /// Sender half of the eject signal. The receiver is owned by the
    /// LB's [`OutlierDetector`].
    eject_tx: mpsc::UnboundedSender<EndpointAddress>,
    /// Receiver moved out exactly once by [`Self::take_eject_rx`].
    eject_rx: Mutex<Option<mpsc::UnboundedReceiver<EndpointAddress>>>,
}

impl OutlierStatsRegistry {
    /// Build a registry with the default RNG.
    pub(crate) fn new(config: OutlierDetectionConfig) -> Arc<Self> {
        Self::with_rng(config, Box::new(FastRandRng))
    }

    /// Build a registry with a custom [`Rng`].
    pub(crate) fn with_rng(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> Arc<Self> {
        let (eject_tx, eject_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            channels: DashMap::new(),
            qualifying_count: AtomicU64::new(0),
            ejected_count: AtomicU64::new(0),
            config,
            rng,
            eject_tx,
            eject_rx: Mutex::new(Some(eject_rx)),
        })
    }

    /// Take the eject-signal receiver. Returns
    /// [`RegistryAlreadyWired`] on a second call — a registry can
    /// drive at most one load balancer.
    fn take_eject_rx(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<EndpointAddress>, RegistryAlreadyWired> {
        self.eject_rx
            .lock()
            .expect("eject_rx mutex poisoned")
            .take()
            .ok_or(RegistryAlreadyWired)
    }

    /// Get or create the state for `addr`. Idempotent — existing
    /// state is preserved across reconnect.
    pub(crate) fn add_channel(&self, addr: EndpointAddress) -> Arc<OutlierChannelState> {
        self.channels
            .entry(addr.clone())
            .or_insert_with(|| Arc::new(OutlierChannelState::new(addr)))
            .clone()
    }

    /// Drop the state for `addr`, decrementing cluster-wide counters
    /// (`qualifying_count`, `ejected_count`) if it was contributing.
    pub(crate) fn remove_channel(&self, addr: &EndpointAddress) {
        if let Some((_, state)) = self.channels.remove(addr) {
            if state.clear_qualifying() {
                self.qualifying_count.fetch_sub(1, Ordering::Relaxed);
            }
            if state.is_ejected() {
                self.ejected_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Number of registered channels.
    pub(crate) fn len(&self) -> usize {
        self.channels.len()
    }

    /// Per-RPC entry point. Records the outcome and, if all gates
    /// pass, transitions the channel to ejected and dispatches the
    /// address on the eject mpsc.
    pub(crate) fn record_outcome(&self, state: &OutlierChannelState, success: bool) {
        if success {
            state.record_success();
        } else {
            state.record_failure();
        }

        let Some(fp) = self.config.failure_percentage.as_ref() else {
            return;
        };

        let (s, f) = state.counters();
        let total = s + f;
        let request_volume = u64::from(fp.request_volume);

        // Bump `qualifying_count` exactly once per channel per
        // interval so the `minimum_hosts` gate is a single atomic load.
        if total >= request_volume && state.mark_qualifying() {
            self.qualifying_count.fetch_add(1, Ordering::Relaxed);
        }

        if state.is_ejected() {
            return;
        }
        if total < request_volume {
            return;
        }
        if self.qualifying_count.load(Ordering::Relaxed) < u64::from(fp.minimum_hosts) {
            return;
        }
        if self.ejected_count.load(Ordering::Relaxed) >= self.max_ejections() {
            return;
        }

        // failure_pct = 100 * failure / total. A50 uses strict ">".
        let failure_pct = 100 * f / total;
        if failure_pct <= u64::from(fp.threshold.get()) {
            return;
        }
        if !roll(&*self.rng, fp.enforcing_failure_percentage.get()) {
            return;
        }

        if state.try_eject(Instant::now()) {
            self.ejected_count.fetch_add(1, Ordering::Relaxed);
            // Send failure (LB receiver dropped during shutdown) is
            // ignored; the registry will be torn down momentarily.
            let _ = self.eject_tx.send(state.addr().clone());
        }
    }

    /// Clear the ejection: flip the state, decrement
    /// `ejected_count`, and decrement the multiplier (gRFC A50
    /// step 6.b: same sweep that un-ejects also decrements). Returns
    /// `true` on the ejected → not-ejected transition.
    pub(crate) fn note_uneject(&self, state: &OutlierChannelState) -> bool {
        if state.try_uneject() {
            self.ejected_count.fetch_sub(1, Ordering::Relaxed);
            state.decrement_multiplier();
            true
        } else {
            false
        }
    }

    /// Time remaining on `state`'s ejection (capped by
    /// `max_ejection_time`). `None` if not ejected;
    /// `Some(Duration::ZERO)` if the deadline has passed (caller
    /// should un-eject rather than start a fresh sleep).
    pub(crate) fn remaining_ejection(
        &self,
        state: &OutlierChannelState,
        now: Instant,
    ) -> Option<Duration> {
        let elapsed = state.ejected_duration(now)?;
        let multiplier = state.ejection_multiplier();
        let cap = self
            .config
            .base_ejection_time
            .max(self.config.max_ejection_time);
        let target = self
            .config
            .base_ejection_time
            .checked_mul(multiplier)
            .unwrap_or(cap)
            .min(cap);
        Some(target.checked_sub(elapsed).unwrap_or_default())
    }

    /// Interval-boundary housekeeping. Resets counters and
    /// decrements multipliers for non-ejected channels. Does not
    /// un-eject — that is driven by each `EjectedChannel`'s timer.
    pub(crate) fn run_housekeeping(&self) {
        for entry in self.channels.iter() {
            let state = entry.value();
            state.snapshot_and_reset();
            if state.clear_qualifying() {
                self.qualifying_count.fetch_sub(1, Ordering::Relaxed);
            }
            if !state.is_ejected() {
                state.decrement_multiplier();
            }
        }
    }

    /// Resolve `max_ejection_percent` against the current channel count.
    fn max_ejections(&self) -> u64 {
        self.channels.len() as u64 * u64::from(self.config.max_ejection_percent.get()) / 100
    }
}

/// Spawn the housekeeping actor. Ticks every `config.interval` and
/// calls [`OutlierStatsRegistry::run_housekeeping`]. Dropping the
/// returned [`AbortOnDrop`] stops the task.
pub(crate) fn spawn_actor(registry: Arc<OutlierStatsRegistry>) -> AbortOnDrop {
    let interval = registry.config.interval;
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            registry.run_housekeeping();
        }
    });
    AbortOnDrop(task)
}

/// Per-LB outlier-detection plumbing: shared registry, eject-signal
/// receiver, and the housekeeping actor handle (aborted on drop). The
/// LB holds this as `Option<OutlierDetector>`.
pub(crate) struct OutlierDetector {
    registry: Arc<OutlierStatsRegistry>,
    eject_rx: mpsc::UnboundedReceiver<EndpointAddress>,
    _actor: AbortOnDrop,
}

impl OutlierDetector {
    /// Take ownership of the registry's eject-signal receiver and
    /// spawn the housekeeping actor. Returns
    /// [`RegistryAlreadyWired`] if the registry is already wired to
    /// another LB.
    pub(crate) fn new(registry: Arc<OutlierStatsRegistry>) -> Result<Self, RegistryAlreadyWired> {
        let eject_rx = registry.take_eject_rx()?;
        let _actor = spawn_actor(registry.clone());
        Ok(Self {
            registry,
            eject_rx,
            _actor,
        })
    }

    /// Shared registry handle.
    pub(crate) fn registry(&self) -> &Arc<OutlierStatsRegistry> {
        &self.registry
    }

    /// Poll for the next address the data path has decided to eject.
    pub(crate) fn poll_eject_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<EndpointAddress>> {
        self.eject_rx.poll_recv(cx)
    }
}

/// Return true with probability `pct / 100` (clamped at 100 ⇒ always).
fn roll(rng: &dyn Rng, pct: u8) -> bool {
    if pct >= 100 {
        return true;
    }
    if pct == 0 {
        return false;
    }
    rng.pct_roll() < u32::from(pct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::outlier_detection::{
        FailurePercentageConfig, OutlierDetectionConfig, Percentage,
    };
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("10.0.0.1", port)
    }

    fn pct(v: u32) -> Percentage {
        Percentage::new(v).unwrap()
    }

    fn base_config() -> OutlierDetectionConfig {
        OutlierDetectionConfig {
            interval: Duration::from_secs(1),
            base_ejection_time: Duration::from_secs(30),
            max_ejection_time: Duration::from_secs(300),
            max_ejection_percent: pct(100),
            success_rate: None,
            failure_percentage: None,
        }
    }

    fn fp_config(
        threshold: u32,
        request_volume: u32,
        minimum_hosts: u32,
    ) -> OutlierDetectionConfig {
        let mut c = base_config();
        c.failure_percentage = Some(FailurePercentageConfig {
            threshold: pct(threshold),
            enforcing_failure_percentage: pct(100),
            minimum_hosts,
            request_volume,
        });
        c
    }

    /// Deterministic RNG: `pct_roll()` returns a fixed value.
    struct FixedRng(AtomicU32);

    impl FixedRng {
        fn boxed(value: u32) -> Box<dyn Rng> {
            Box::new(Self(AtomicU32::new(value)))
        }
    }

    impl Rng for FixedRng {
        fn pct_roll(&self) -> u32 {
            self.0.load(Ordering::Relaxed)
        }
    }

    /// Drive `n` outcomes through `record_outcome` for one channel.
    fn drive(
        registry: &OutlierStatsRegistry,
        state: &OutlierChannelState,
        successes: u64,
        failures: u64,
    ) {
        for _ in 0..successes {
            registry.record_outcome(state, true);
        }
        for _ in 0..failures {
            registry.record_outcome(state, false);
        }
    }

    // ----- record_outcome: failure-percentage detection -----

    #[test]
    fn ejects_above_threshold_inline() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
        }
        drive(&registry, &bad, 10, 90);
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn skips_below_threshold() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            // 30% failure → below 50% threshold.
            drive(&registry, &s, 70, 30);
            all.push(s);
        }
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn at_threshold_does_not_eject() {
        // A50 specifies a strict "greater than" comparison.
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(0));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 50, 50);
            all.push(s);
        }
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn minimum_hosts_gates_ejection() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 5), FixedRng::boxed(99));
        // Only 2 hosts have request_volume ≥ 10; minimum_hosts is 5 ⇒ skip.
        let mut all = vec![];
        for port in 8080..=8081 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 0, 100);
            all.push(s);
        }
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn request_volume_filters_low_traffic() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 100, 3), FixedRng::boxed(99));
        let bad = registry.add_channel(addr(8080));
        drive(&registry, &bad, 0, 5);
        for port in 8081..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 200, 0);
        }
        assert!(!bad.is_ejected());
    }

    #[test]
    fn enforcement_zero_percent_never_ejects() {
        let mut config = fp_config(50, 10, 3);
        config
            .failure_percentage
            .as_mut()
            .unwrap()
            .enforcing_failure_percentage = pct(0);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(0));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 0, 100);
            all.push(s);
        }
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn max_ejection_percent_caps_concurrent_ejections() {
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(20);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));

        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            all.push(s);
        }
        // Drive all hosts to bad state in parallel pseudo-order.
        for s in &all {
            drive(&registry, s, 0, 100);
        }

        let ejected = all.iter().filter(|s| s.is_ejected()).count();
        // 5 hosts × 20% = 1 max ejection.
        assert_eq!(ejected, 1);
    }

    #[test]
    fn remove_channel_decrements_counters() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let mut all = vec![];
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
            all.push(s);
        }
        let bad = registry.add_channel(addr(8084));
        drive(&registry, &bad, 0, 100);
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
        // Each healthy host crossed request_volume; bad too. So
        // qualifying_count = 5.
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 5);

        registry.remove_channel(&addr(8084));
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 0);
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn ejection_dispatches_address_through_mpsc() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let mut rx = registry.take_eject_rx().expect("receiver available");
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
        }
        drive(&registry, &bad, 10, 90);

        // Eject dispatched exactly once via the mpsc.
        assert_eq!(rx.try_recv(), Ok(addr(8084)));
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    // ----- Housekeeping -----

    #[test]
    fn housekeeping_resets_counters_and_qualifying() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
        }
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 4);

        registry.run_housekeeping();
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 0);
        for port in 8080..=8083 {
            let s = registry.channels.get(&addr(port)).unwrap();
            assert_eq!(s.counters(), (0, 0));
        }
    }

    #[test]
    fn housekeeping_decrements_multiplier_on_healthy_interval() {
        let registry = OutlierStatsRegistry::with_rng(base_config(), FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        // Force multiplier to 3 directly (no traffic, no eject).
        s.set_ejection_multiplier(3);

        registry.run_housekeeping();
        assert_eq!(s.ejection_multiplier(), 2);
    }

    #[test]
    fn housekeeping_leaves_ejected_multipliers_alone() {
        let registry = OutlierStatsRegistry::with_rng(base_config(), FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        s.try_eject(Instant::now());
        s.set_ejection_multiplier(3);

        registry.run_housekeeping();
        // Ejected channels keep their multiplier; un-ejection is the
        // LB's job (timer-driven via EjectedChannel).
        assert_eq!(s.ejection_multiplier(), 3);
        assert!(s.is_ejected());
    }

    // ----- remaining_ejection / note_uneject -----

    #[test]
    fn remaining_ejection_returns_full_duration_for_fresh_eject() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        let t0 = Instant::now();
        s.try_eject(t0);
        // Multiplier is 1 after the first eject, so target = 10s.
        let remaining = registry.remaining_ejection(&s, t0).unwrap();
        assert_eq!(remaining, Duration::from_secs(10));
    }

    #[test]
    fn remaining_ejection_capped_at_max_ejection_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(15);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        let t0 = Instant::now();
        s.try_eject(t0);
        s.set_ejection_multiplier(10); // base * 10 = 100s, but cap = 15s.
        let remaining = registry.remaining_ejection(&s, t0).unwrap();
        assert_eq!(remaining, Duration::from_secs(15));
    }

    #[test]
    fn remaining_ejection_subtracts_elapsed_for_re_discovery() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(30);
        config.max_ejection_time = Duration::from_secs(60);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        let t0 = Instant::now();
        s.try_eject(t0);
        // Re-discovered 10s into the ejection — should still have 20s left.
        let remaining = registry
            .remaining_ejection(&s, t0 + Duration::from_secs(10))
            .unwrap();
        assert_eq!(remaining, Duration::from_secs(20));
    }

    #[test]
    fn remaining_ejection_zero_past_deadline() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        let t0 = Instant::now();
        s.try_eject(t0);
        // 60s have passed but target is 10s — caller should un-eject.
        let remaining = registry
            .remaining_ejection(&s, t0 + Duration::from_secs(60))
            .unwrap();
        assert_eq!(remaining, Duration::ZERO);
    }

    #[test]
    fn remaining_ejection_none_when_not_ejected() {
        let registry = OutlierStatsRegistry::with_rng(base_config(), FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        assert!(registry.remaining_ejection(&s, Instant::now()).is_none());
    }

    #[test]
    fn note_uneject_clears_state_and_decrements_counter() {
        let registry = OutlierStatsRegistry::with_rng(base_config(), FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        s.try_eject(Instant::now()); // bumps multiplier 0 → 1
        registry.ejected_count.fetch_add(1, Ordering::Relaxed);
        assert!(s.is_ejected());
        assert_eq!(s.ejection_multiplier(), 1);

        assert!(registry.note_uneject(&s));
        assert!(!s.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 0);
        // A50 step 6.b: same sweep that un-ejects also decrements
        // the multiplier.
        assert_eq!(s.ejection_multiplier(), 0);

        // Second call is a no-op.
        assert!(!registry.note_uneject(&s));
        assert_eq!(s.ejection_multiplier(), 0);
    }

    /// A50 step 6.b: un-eject and multiplier decrement happen at the
    /// same sweep. Re-eject right after un-eject must size the
    /// backoff with the *decremented* multiplier.
    #[test]
    fn re_eject_after_uneject_uses_fresh_multiplier() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(300);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));

        let t0 = Instant::now();
        s.try_eject(t0); // multiplier 0 → 1
        registry.ejected_count.fetch_add(1, Ordering::Relaxed);
        assert_eq!(s.ejection_multiplier(), 1);

        // Backoff elapses; LB calls note_uneject.
        registry.note_uneject(&s);
        assert_eq!(s.ejection_multiplier(), 0);

        // Channel immediately misbehaves again and gets re-ejected.
        let t1 = t0 + Duration::from_secs(11);
        s.try_eject(t1); // multiplier 0 → 1, not 1 → 2
        assert_eq!(s.ejection_multiplier(), 1);
        // Remaining ejection duration should be `base * 1 = 10s`,
        // not `base * 2 = 20s`.
        assert_eq!(
            registry.remaining_ejection(&s, t1).unwrap(),
            Duration::from_secs(10),
        );
    }

    // ----- Spawned actor -----
    //
    // The actor's algorithmic behavior is fully exercised by the
    // synchronous `housekeeping_*` tests above; here we only verify
    // that dropping the `AbortOnDrop` handle reliably stops the task.

    #[tokio::test(start_paused = true)]
    async fn dropping_abort_stops_actor() {
        let mut config = base_config();
        config.interval = Duration::from_millis(50);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        s.set_ejection_multiplier(5);

        let abort = spawn_actor(registry.clone());
        drop(abort);

        // Even with several tick periods elapsed, no housekeeping
        // should have run because the task was aborted.
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;

        assert_eq!(s.ejection_multiplier(), 5);
    }

    // ----- OutlierChannelState sanity (kept in this file as it is the
    //       primary consumer of the type) -----

    #[test]
    fn channel_state_records_and_resets() {
        let s = OutlierChannelState::new(addr(8080));
        s.record_success();
        s.record_success();
        s.record_failure();
        assert_eq!(s.snapshot_and_reset(), (2, 1));
        assert_eq!(s.snapshot_and_reset(), (0, 0));
    }

    #[test]
    fn channel_state_try_eject_uneject_transitions_atomically() {
        let s = OutlierChannelState::new(addr(8080));
        assert!(!s.is_ejected());
        assert!(s.try_eject(Instant::now()));
        assert!(s.is_ejected());
        // Second call is a no-op.
        assert!(!s.try_eject(Instant::now()));
        assert!(s.try_uneject());
        assert!(!s.is_ejected());
        assert!(!s.try_uneject());
    }

    #[test]
    fn channel_state_remembers_its_address() {
        let s = OutlierChannelState::new(addr(9090));
        assert_eq!(s.addr(), &addr(9090));
    }
}
