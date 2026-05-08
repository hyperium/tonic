//! gRFC A50 outlier detection.
//!
//! The algorithm is split between the data path and a spawned actor:
//!
//! - **Per-RPC detection** runs inline on each call completion via
//!   [`OutlierStatsRegistry::record_outcome`]. The wrapper records the
//!   outcome on the channel's [`OutlierChannelState`], evaluates the
//!   failure-percentage threshold against the channel's local
//!   counters, and ejects the channel directly by flipping its
//!   `watch::Sender<bool>`. Cluster-wide gates (`minimum_hosts`,
//!   `max_ejection_percent`) are enforced via two atomic counters on
//!   the registry, kept in sync as channels cross thresholds.
//! - **Interval-based housekeeping** runs in a spawned actor (see
//!   [`spawn_actor`]). It resets per-channel counters at the
//!   `config.interval` boundary, un-ejects channels whose
//!   `base × multiplier` backoff has elapsed, and decrements
//!   multipliers for non-ejected channels. The actor never makes
//!   ejection decisions.
//!
//! `LoadBalancer::poll_ready` observes ejections in O(1) per
//! transition by polling a `FuturesUnordered<watch::Receiver::changed()>`
//! over each channel's signal.
//!
//! Only the failure-percentage algorithm is dispatched. The
//! success-rate algorithm (cross-endpoint mean/stdev) is left to a
//! follow-up.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::channel_state::OutlierChannelState;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::OutlierDetectionConfig;

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
/// concurrently by:
/// - The load balancer's call wrapper, which calls
///   [`Self::record_outcome`] after each RPC completion.
/// - The spawned actor task, which calls [`Self::run_housekeeping`]
///   on every `config.interval` tick.
/// - The load balancer's `poll_ready`, which subscribes to per-channel
///   ejection signals via [`OutlierChannelState::subscribe`].
pub(crate) struct OutlierStatsRegistry {
    /// Per-endpoint state, keyed by address. Inserted by the LB on
    /// channel creation and removed on disconnect.
    channels: DashMap<EndpointAddress, Arc<OutlierChannelState>>,
    /// Number of channels currently with `total >= request_volume` in
    /// the active interval. Drives the `minimum_hosts` gate.
    qualifying_count: AtomicU64,
    /// Number of channels currently ejected. Drives the
    /// `max_ejection_percent` cap.
    ejected_count: AtomicU64,
    config: OutlierDetectionConfig,
    rng: Box<dyn Rng>,
}

impl OutlierStatsRegistry {
    /// Build a registry with the default RNG.
    pub(crate) fn new(config: OutlierDetectionConfig) -> Arc<Self> {
        Self::with_rng(config, Box::new(FastRandRng))
    }

    /// Build a registry with a custom [`Rng`].
    pub(crate) fn with_rng(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> Arc<Self> {
        Arc::new(Self {
            channels: DashMap::new(),
            qualifying_count: AtomicU64::new(0),
            ejected_count: AtomicU64::new(0),
            config,
            rng,
        })
    }

    /// Register a new channel. Returns the `Arc<OutlierChannelState>`
    /// the load balancer wires into the channel; the same `Arc` is
    /// retained in the registry so the actor can iterate it.
    pub(crate) fn add_channel(&self, addr: EndpointAddress) -> Arc<OutlierChannelState> {
        let state = Arc::new(OutlierChannelState::new());
        self.channels.insert(addr, state.clone());
        state
    }

    /// Forget a channel. Drops the registry's reference; cluster-wide
    /// counters are decremented if the channel was qualifying or
    /// ejected.
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

    /// Per-RPC entry point. Called by the load balancer's call wrapper
    /// after each RPC completion. Increments the channel's success or
    /// failure counter and then evaluates the failure-percentage
    /// threshold; if all gates pass, ejects the channel inline.
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

        // Track when each channel first qualifies in the current
        // interval, so the `minimum_hosts` gate can be checked with a
        // single atomic load.
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
        }
    }

    /// Interval-boundary housekeeping. Called by the spawned actor on
    /// each `config.interval` tick. Resets counters, un-ejects
    /// channels whose backoff has elapsed, and decrements multipliers
    /// for non-ejected channels.
    pub(crate) fn run_housekeeping(&self, now: Instant) {
        // Cap the un-ejection backoff at `max(base, max_ejection_time)`.
        let cap = self
            .config
            .base_ejection_time
            .max(self.config.max_ejection_time);

        for entry in self.channels.iter() {
            let state = entry.value();

            // Reset counters; clear `is_qualifying` and adjust the
            // registry-level counter in lockstep.
            state.snapshot_and_reset();
            if state.clear_qualifying() {
                self.qualifying_count.fetch_sub(1, Ordering::Relaxed);
            }

            if state.is_ejected() {
                let multiplier = state.ejection_multiplier();
                let elapsed = state.ejected_duration(now).unwrap_or_default();
                if let Some(scaled) = self.config.base_ejection_time.checked_mul(multiplier)
                    && elapsed >= scaled.min(cap)
                    && state.try_uneject()
                {
                    self.ejected_count.fetch_sub(1, Ordering::Relaxed);
                }
            } else {
                state.decrement_multiplier();
            }
        }
    }

    /// `max_ejection_percent` resolved against the current channel
    /// count. Updated as channels come and go.
    fn max_ejections(&self) -> u64 {
        self.channels.len() as u64 * u64::from(self.config.max_ejection_percent.get()) / 100
    }
}

/// Spawn the housekeeping actor. The task ticks every
/// `config.interval` and calls
/// [`OutlierStatsRegistry::run_housekeeping`]. Dropping the returned
/// [`AbortOnDrop`] stops the task.
pub(crate) fn spawn_actor(registry: Arc<OutlierStatsRegistry>) -> AbortOnDrop {
    let interval = registry.config.interval;
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            registry.run_housekeeping(Instant::now());
        }
    });
    AbortOnDrop(task)
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

    // ----- Housekeeping -----

    #[test]
    fn housekeeping_resets_counters_and_qualifying() {
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
        }
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 4);

        registry.run_housekeeping(Instant::now());
        assert_eq!(registry.qualifying_count.load(Ordering::Relaxed), 0);
        for port in 8080..=8083 {
            let s = registry.channels.get(&addr(port)).unwrap();
            assert_eq!(s.counters(), (0, 0));
        }
    }

    #[test]
    fn housekeeping_unejects_after_base_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));

        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&registry, &s, 100, 0);
        }
        drive(&registry, &bad, 0, 100);
        assert!(bad.is_ejected());

        // Advance fewer than base_ejection_time ⇒ stays ejected.
        let t0 = Instant::now();
        registry.run_housekeeping(t0 + Duration::from_secs(9));
        assert!(bad.is_ejected());

        // After base_ejection_time × 1 elapsed ⇒ uneject.
        registry.run_housekeeping(t0 + Duration::from_secs(20));
        assert!(!bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn housekeeping_decrements_multiplier_on_healthy_interval() {
        let registry = OutlierStatsRegistry::with_rng(base_config(), FixedRng::boxed(99));
        let s = registry.add_channel(addr(8080));
        // Force multiplier to 3 directly (no traffic, no eject).
        s.set_ejection_multiplier(3);

        registry.run_housekeeping(Instant::now());
        assert_eq!(s.ejection_multiplier(), 2);
    }

    #[test]
    fn housekeeping_caps_ejection_at_max_ejection_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(15);
        let registry = OutlierStatsRegistry::with_rng(config, FixedRng::boxed(99));

        let s = registry.add_channel(addr(8080));
        // Pretend 8080 was ejected long ago with a huge multiplier.
        s.try_eject(Instant::now());
        s.set_ejection_multiplier(10);
        registry.ejected_count.fetch_add(0, Ordering::Relaxed); // try_eject already added 1

        // base * multiplier = 100s, but cap = 15s. Sweep at 16s ⇒ uneject.
        let t0 = Instant::now();
        registry.run_housekeeping(t0 + Duration::from_secs(16));
        assert!(!s.is_ejected());
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
        let s = OutlierChannelState::new();
        s.record_success();
        s.record_success();
        s.record_failure();
        assert_eq!(s.snapshot_and_reset(), (2, 1));
        assert_eq!(s.snapshot_and_reset(), (0, 0));
    }

    #[test]
    fn channel_state_try_eject_uneject_flips_signal() {
        let s = OutlierChannelState::new();
        assert!(!s.is_ejected());
        assert!(s.try_eject(Instant::now()));
        assert!(s.is_ejected());
        // Second call is a no-op.
        assert!(!s.try_eject(Instant::now()));
        assert!(s.try_uneject());
        assert!(!s.is_ejected());
        assert!(!s.try_uneject());
    }
}
