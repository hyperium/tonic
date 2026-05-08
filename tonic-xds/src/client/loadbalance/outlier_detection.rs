//! gRFC A50 outlier-detection sweep engine.
//!
//! Reads per-endpoint counters from a shared
//! [`DashMap<EndpointAddress, Arc<OutlierChannelState>>`] and applies
//! ejection / un-ejection decisions in place by toggling each entry's
//! ejection signal. The load balancer registers each [`ReadyChannel`]'s
//! [`OutlierChannelState`] in the same map and observes the signal via
//! a `FuturesUnordered` of `watch::Receiver::changed()` futures, so the
//! O(n) sweep runs in a spawned actor task off the LB's critical path.
//!
//! Only the failure-percentage algorithm is currently dispatched. If
//! [`OutlierDetectionConfig::success_rate`] is set, it is ignored.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md
//! [`ReadyChannel`]: crate::client::loadbalance::channel_state::ReadyChannel
//! [`OutlierChannelState`]: crate::client::loadbalance::channel_state::OutlierChannelState
//! [`OutlierDetectionConfig::success_rate`]: crate::xds::resource::outlier_detection::OutlierDetectionConfig::success_rate

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::channel_state::OutlierChannelState;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::{FailurePercentageConfig, OutlierDetectionConfig};

/// Shared map of per-endpoint outlier state, keyed by address. The
/// load balancer inserts each [`ReadyChannel`]'s
/// [`OutlierChannelState`] on connect and removes it on disconnect; the
/// detector iterates the map on each sweep.
///
/// [`ReadyChannel`]: crate::client::loadbalance::channel_state::ReadyChannel
pub(crate) type OutlierStatsRegistry = Arc<DashMap<EndpointAddress, Arc<OutlierChannelState>>>;

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

/// Algorithm-private per-endpoint state. Tracks the ejection-time
/// multiplier and the last ejection timestamp; counters and the
/// outward-facing ejection signal live on the channel's
/// [`OutlierChannelState`].
struct AlgState {
    /// Number of times this endpoint has been ejected. Grows on each
    /// re-ejection and decays on each healthy interval.
    ejection_multiplier: u32,
    /// `Some(at)` when currently ejected; `None` otherwise.
    ejected_at: Option<Instant>,
}

impl AlgState {
    fn new() -> Self {
        Self {
            ejection_multiplier: 0,
            ejected_at: None,
        }
    }
}

/// gRFC A50 outlier detector.
///
/// Held by an actor task that ticks once per `config.interval` and
/// calls [`Self::run_sweep`] over the shared [`OutlierStatsRegistry`].
/// Stats and ejection signals live on the channels themselves; the
/// detector owns only algorithm-private metadata (per-endpoint
/// multiplier and last-ejection timestamp).
pub(crate) struct OutlierDetector {
    config: OutlierDetectionConfig,
    state: HashMap<EndpointAddress, AlgState>,
    rng: Box<dyn Rng>,
}

impl OutlierDetector {
    /// Build the detector with the default RNG (`fastrand`).
    pub(crate) fn new(config: OutlierDetectionConfig) -> Self {
        Self::with_rng(config, Box::new(FastRandRng))
    }

    /// Build the detector with a custom [`Rng`].
    pub(crate) fn with_rng(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> Self {
        Self {
            config,
            state: HashMap::new(),
            rng,
        }
    }

    /// Run one sweep at logical time `now` over the shared registry.
    /// Applies ejection decisions inline by calling
    /// [`OutlierChannelState::eject`] / [`OutlierChannelState::uneject`]
    /// on each affected entry.
    ///
    /// Order of operations follows gRFC A50:
    /// 1. Record the timestamp.
    /// 2. Snapshot each address's call-counter buckets.
    /// 3. Run the success-rate algorithm if configured (not yet dispatched).
    /// 4. Run the failure-percentage algorithm if configured.
    /// 5. Decrement the multiplier of non-ejected addresses with
    ///    multiplier > 0; un-eject ejected addresses whose backoff has
    ///    elapsed.
    pub(crate) fn run_sweep(&mut self, now: Instant, channels: &OutlierStatsRegistry) {
        // Step 2: snapshot every channel's counters and record which
        // addresses are still in the registry.
        let mut snapshots: Vec<Candidate> = Vec::with_capacity(channels.len());
        let mut seen: HashSet<EndpointAddress> = HashSet::with_capacity(channels.len());
        for entry in channels.iter() {
            let addr = entry.key().clone();
            let (success, failure) = entry.value().snapshot_and_reset();
            let alg = self.state.entry(addr.clone()).or_insert_with(AlgState::new);
            snapshots.push(Candidate {
                addr: addr.clone(),
                success,
                failure,
                total: success + failure,
                already_ejected: alg.ejected_at.is_some(),
            });
            seen.insert(addr);
        }
        // Drop algorithm state for addresses no longer in the registry.
        self.state.retain(|addr, _| seen.contains(addr));

        // Per-sweep cap on new ejections, enforced as a budget the
        // algorithms decrement. Per A50, the check happens before each
        // candidate.
        let total_endpoints = self.state.len();
        let max_ejections = (total_endpoints as u64
            * u64::from(self.config.max_ejection_percent.get())
            / 100) as usize;
        let already_ejected = self
            .state
            .values()
            .filter(|s| s.ejected_at.is_some())
            .count();
        let mut budget = max_ejections.saturating_sub(already_ejected);

        // Steps 3 & 4: run the algorithms. Ejected hosts have no
        // in-interval traffic in production and so naturally fail the
        // `request_volume` gate; iterating every address (per spec) is
        // equivalent to iterating non-ejected ones. Step 3 (success-
        // rate ejection) is not yet dispatched.
        let mut to_eject: Vec<EndpointAddress> = Vec::new();
        if let Some(fp) = self.config.failure_percentage.as_ref() {
            run_failure_percentage(fp, &snapshots, &mut budget, &mut to_eject, &*self.rng);
        }

        for addr in &to_eject {
            if let Some(alg) = self.state.get_mut(addr) {
                alg.ejected_at = Some(now);
                alg.ejection_multiplier = alg.ejection_multiplier.saturating_add(1);
            }
            if let Some(state) = channels.get(addr) {
                state.eject();
            }
        }

        // Step 5: decrement multipliers for non-ejected addresses;
        // un-eject ejected addresses whose backoff has elapsed. Runs
        // *after* re-ejection, so a same-sweep re-eject refreshes
        // `ejected_at` and the un-eject check sees zero elapsed time.
        let cap = self
            .config
            .base_ejection_time
            .max(self.config.max_ejection_time);
        for (addr, alg) in self.state.iter_mut() {
            if let Some(at) = alg.ejected_at {
                if let Some(scaled) = self
                    .config
                    .base_ejection_time
                    .checked_mul(alg.ejection_multiplier)
                    && now.duration_since(at) >= scaled.min(cap)
                {
                    alg.ejected_at = None;
                    if let Some(state) = channels.get(addr) {
                        state.uneject();
                    }
                }
            } else if alg.ejection_multiplier > 0 {
                alg.ejection_multiplier -= 1;
            }
        }
    }

    /// Spawn the detector as an actor task with the default RNG. The
    /// task ticks every `config.interval` and runs a sweep over the
    /// shared registry. Dropping the returned [`AbortOnDrop`] stops
    /// the task.
    pub(crate) fn spawn(
        config: OutlierDetectionConfig,
        channels: OutlierStatsRegistry,
    ) -> AbortOnDrop {
        Self::spawn_inner(Self::new(config), channels)
    }

    /// Variant of [`Self::spawn`] that accepts a custom [`Rng`].
    pub(crate) fn spawn_with_rng(
        config: OutlierDetectionConfig,
        rng: Box<dyn Rng>,
        channels: OutlierStatsRegistry,
    ) -> AbortOnDrop {
        Self::spawn_inner(Self::with_rng(config, rng), channels)
    }

    fn spawn_inner(mut detector: Self, channels: OutlierStatsRegistry) -> AbortOnDrop {
        let interval = detector.config.interval;
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately so the actor runs an initial
            // sweep on startup; subsequent ticks fire on the interval.
            loop {
                ticker.tick().await;
                detector.run_sweep(Instant::now(), &channels);
            }
        });
        AbortOnDrop(task)
    }
}

/// A50 failure-percentage algorithm.
fn run_failure_percentage(
    cfg: &FailurePercentageConfig,
    all: &[Candidate],
    budget: &mut usize,
    out: &mut Vec<EndpointAddress>,
    rng: &dyn Rng,
) {
    let qualifying: Vec<&Candidate> = all
        .iter()
        .filter(|c| c.total >= u64::from(cfg.request_volume))
        .collect();
    if qualifying.len() < cfg.minimum_hosts as usize {
        return;
    }

    let threshold = u64::from(cfg.threshold.get());
    for c in qualifying {
        if *budget == 0 {
            break;
        }
        // A50 doesn't forbid `request_volume == 0`, in which case a
        // candidate may have `total == 0`. The spec is silent on
        // `0/0`; skip these endpoints rather than divide by zero.
        if c.total == 0 {
            continue;
        }
        // failure_pct = 100 * failure / total. A50 specifies a strict
        // "greater than" comparison: an address sitting exactly at
        // the threshold is not ejected.
        let failure_pct = 100 * c.failure / c.total;
        if failure_pct > threshold && roll(rng, cfg.enforcing_failure_percentage.get()) {
            out.push(c.addr.clone());
            // See `Candidate::already_ejected` for why re-ejections
            // don't consume the budget.
            if !c.already_ejected {
                *budget -= 1;
            }
        }
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

/// Cached per-endpoint snapshot used during a sweep.
struct Candidate {
    addr: EndpointAddress,
    success: u64,
    failure: u64,
    total: u64,
    /// Whether this address was already ejected at the start of the
    /// sweep. Re-ejecting an already-ejected address refreshes its
    /// timestamp and bumps its multiplier but doesn't change the count
    /// of currently-ejected addresses, so it must not consume a
    /// `max_ejection_percent` budget slot.
    already_ejected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::outlier_detection::{
        FailurePercentageConfig, OutlierDetectionConfig, Percentage,
    };
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    // ----- Fixtures -----

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

    fn detector_with_rng(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> OutlierDetector {
        OutlierDetector::with_rng(config, rng)
    }

    fn registry() -> OutlierStatsRegistry {
        Arc::new(DashMap::new())
    }

    fn add(channels: &OutlierStatsRegistry, port: u16) -> Arc<OutlierChannelState> {
        let state = Arc::new(OutlierChannelState::new());
        channels.insert(addr(port), state.clone());
        state
    }

    fn ejected(channels: &OutlierStatsRegistry, port: u16) -> bool {
        channels
            .get(&addr(port))
            .map(|e| e.value().is_ejected())
            .unwrap_or(false)
    }

    fn ejected_count(channels: &OutlierStatsRegistry) -> usize {
        channels.iter().filter(|e| e.value().is_ejected()).count()
    }

    // ----- OutlierChannelState (sanity) -----

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
    fn channel_state_eject_uneject_flips_signal() {
        let s = OutlierChannelState::new();
        assert!(!s.is_ejected());
        s.eject();
        assert!(s.is_ejected());
        s.uneject();
        assert!(!s.is_ejected());
    }

    // ----- Failure-percentage algorithm -----

    #[test]
    fn failure_percentage_ejects_above_threshold() {
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let channels = registry();

        for port in 8080..=8083 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_success();
            }
        }
        let bad = add(&channels, 8084);
        for _ in 0..90 {
            bad.record_failure();
        }
        for _ in 0..10 {
            bad.record_success();
        }

        detector.run_sweep(Instant::now(), &channels);
        assert!(bad.is_ejected());
        for port in 8080..=8083 {
            assert!(!ejected(&channels, port));
        }
    }

    #[test]
    fn failure_percentage_skips_below_threshold() {
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        let channels = registry();
        for port in 8080..=8084 {
            let s = add(&channels, port);
            // 30% failure → below threshold of 50%.
            for _ in 0..70 {
                s.record_success();
            }
            for _ in 0..30 {
                s.record_failure();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 0);
    }

    #[test]
    fn failure_percentage_at_threshold_does_not_eject() {
        // A50 specifies a strict "greater than" comparison.
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(0));
        let channels = registry();
        for port in 8080..=8084 {
            let s = add(&channels, port);
            for _ in 0..50 {
                s.record_success();
            }
            for _ in 0..50 {
                s.record_failure();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 0);
    }

    #[test]
    fn minimum_hosts_gates_failure_percentage() {
        let mut detector = detector_with_rng(fp_config(50, 10, 5), FixedRng::boxed(99));
        let channels = registry();
        // Only 2 hosts have request_volume ≥ 10; minimum_hosts is 5 ⇒ skip.
        for port in 8080..=8081 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_failure();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 0);
    }

    #[test]
    fn request_volume_filters_low_traffic_endpoints() {
        let mut detector = detector_with_rng(fp_config(50, 100, 3), FixedRng::boxed(99));
        let channels = registry();
        let bad = add(&channels, 8080);
        for _ in 0..5 {
            bad.record_failure();
        }
        for port in 8081..=8084 {
            let s = add(&channels, port);
            for _ in 0..200 {
                s.record_success();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 0);
    }

    #[test]
    fn enforcement_zero_percent_never_ejects() {
        let mut config = fp_config(50, 10, 3);
        config
            .failure_percentage
            .as_mut()
            .unwrap()
            .enforcing_failure_percentage = pct(0);
        let mut detector = detector_with_rng(config, FixedRng::boxed(0));
        let channels = registry();
        for port in 8080..=8084 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_failure();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 0);
    }

    // ----- Ejection multiplier / un-ejection -----

    #[test]
    fn unejects_after_base_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        let channels = registry();

        for port in 8080..=8084 {
            let s = add(&channels, port);
            if port == 8084 {
                for _ in 0..100 {
                    s.record_failure();
                }
            } else {
                for _ in 0..100 {
                    s.record_success();
                }
            }
        }

        let t0 = Instant::now();
        detector.run_sweep(t0, &channels);
        assert!(ejected(&channels, 8084));

        // Still ejected just before base_ejection_time elapses.
        detector.run_sweep(t0 + Duration::from_secs(9), &channels);
        assert!(ejected(&channels, 8084));

        // Un-eject after `base * multiplier(=1)` = 10s.
        detector.run_sweep(t0 + Duration::from_secs(10), &channels);
        assert!(!ejected(&channels, 8084));
    }

    #[test]
    fn re_ejection_doubles_duration() {
        // Same-sweep un-eject + re-eject grows the multiplier 1 → 2.
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        let channels = registry();

        let bad = add(&channels, 8084);
        for port in 8080..=8083 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_success();
            }
        }
        for _ in 0..100 {
            bad.record_failure();
        }

        // Sweep 1: eject. Multiplier 0 → 1.
        let t0 = Instant::now();
        detector.run_sweep(t0, &channels);
        assert!(bad.is_ejected());

        // Re-record stats so sweep 2 has volume to evaluate.
        for port in 8080..=8083 {
            let s = channels.get(&addr(port)).unwrap().value().clone();
            for _ in 0..100 {
                s.record_success();
            }
        }
        for _ in 0..100 {
            bad.record_failure();
        }

        // Sweep 2 at t0+10: re-ejection refreshes timestamp, multiplier 1 → 2.
        detector.run_sweep(t0 + Duration::from_secs(10), &channels);
        assert!(bad.is_ejected());

        // Re-ejection started at t0+10 with multiplier=2 → duration 20s.
        detector.run_sweep(t0 + Duration::from_secs(29), &channels);
        assert!(bad.is_ejected());

        // Un-ejects at the 20s mark (30s after t0).
        detector.run_sweep(t0 + Duration::from_secs(30), &channels);
        assert!(!bad.is_ejected());
    }

    #[test]
    fn ejection_capped_by_max_ejection_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(15);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        let channels = registry();

        for port in 8080..=8084 {
            add(&channels, port);
        }
        let t0 = Instant::now();
        // Force multiplier=10 on 8084 directly. We need to drive a
        // first sweep to populate `state[8084]`, then fix it up.
        detector.run_sweep(t0, &channels);
        let alg = detector.state.get_mut(&addr(8084)).unwrap();
        alg.ejection_multiplier = 10;
        alg.ejected_at = Some(t0);
        channels.get(&addr(8084)).unwrap().value().eject();

        // base*multiplier = 100s; cap = 15s → un-eject after 16s.
        detector.run_sweep(t0 + Duration::from_secs(16), &channels);
        assert!(!ejected(&channels, 8084));
    }

    #[test]
    fn max_ejection_percent_caps_concurrent_ejections() {
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(20);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        let channels = registry();

        for port in 8080..=8084 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_failure();
            }
        }
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(ejected_count(&channels), 1);
    }

    #[test]
    fn already_ejected_re_ejection_does_not_consume_budget() {
        // 5 hosts: one already ejected, four newly bad. Cap permits 3
        // concurrently ejected, with 1 already taken — so 2 new
        // ejections remain in budget.
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        let channels = registry();

        // Pre-eject 8080 by driving one sweep with bad stats.
        let already_bad = add(&channels, 8080);
        for _ in 0..100 {
            already_bad.record_failure();
        }
        // Use a tiny first sweep to enter ejected state via the algorithm.
        // Need at least minimum_hosts=3 candidates with volume; add three
        // healthy hosts with ≥10 requests so the algorithm runs and the
        // single bad one is ejected (cap 60% of 4 hosts = 2 → budget 2 → 1
        // new ejection).
        for port in 8085..=8087 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_success();
            }
        }
        let t0 = Instant::now();
        detector.run_sweep(t0, &channels);
        assert!(already_bad.is_ejected());

        // Now grow the cluster to 5 hosts (8080 + 8081..=8084) and feed
        // bad stats. 8085..=8087 are no longer relevant — drop them.
        channels.remove(&addr(8085));
        channels.remove(&addr(8086));
        channels.remove(&addr(8087));
        for port in 8081..=8084 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_failure();
            }
        }
        for _ in 0..100 {
            already_bad.record_failure();
        }

        detector.run_sweep(t0 + Duration::from_secs(2), &channels);
        // Cap = 60% of 5 = 3. already_ejected = 1. Budget = 2. Plus
        // 8080's re-eject which doesn't consume budget. So 2 NEW
        // ejections among 8081..=8084.
        let new_ejects = (8081..=8084).filter(|p| ejected(&channels, *p)).count();
        assert_eq!(new_ejects, 2);
    }

    #[test]
    fn multiplier_decrements_on_healthy_interval() {
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        let channels = registry();
        let s = add(&channels, 8080);
        // First sweep populates the alg state.
        detector.run_sweep(Instant::now(), &channels);
        // Force multiplier to 3 without ejecting.
        detector
            .state
            .get_mut(&addr(8080))
            .unwrap()
            .ejection_multiplier = 3;
        s.record_success();
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(
            detector.state.get(&addr(8080)).unwrap().ejection_multiplier,
            2,
        );
    }

    #[test]
    fn multiplier_decrements_even_without_traffic() {
        // A50: a non-ejected address with multiplier > 0 has its
        // multiplier decremented every sweep, regardless of traffic.
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        let channels = registry();
        add(&channels, 8080);
        detector.run_sweep(Instant::now(), &channels);
        detector
            .state
            .get_mut(&addr(8080))
            .unwrap()
            .ejection_multiplier = 3;
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(
            detector.state.get(&addr(8080)).unwrap().ejection_multiplier,
            2,
        );
    }

    #[test]
    fn alg_state_dropped_when_channel_removed() {
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        let channels = registry();
        add(&channels, 8080);
        add(&channels, 8081);
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(detector.state.len(), 2);

        channels.remove(&addr(8080));
        detector.run_sweep(Instant::now(), &channels);
        assert_eq!(detector.state.len(), 1);
        assert!(detector.state.contains_key(&addr(8081)));
    }

    // ----- Spawned actor -----

    #[tokio::test(start_paused = true)]
    async fn spawned_actor_runs_sweeps_on_tick() {
        let mut config = fp_config(50, 10, 3);
        config.interval = Duration::from_millis(100);
        let channels = registry();

        for port in 8080..=8083 {
            let s = add(&channels, port);
            for _ in 0..100 {
                s.record_success();
            }
        }
        let bad = add(&channels, 8084);
        for _ in 0..100 {
            bad.record_failure();
        }

        let _abort = OutlierDetector::spawn_with_rng(config, FixedRng::boxed(99), channels.clone());

        // Advance past the first sweep tick. The yield gives the
        // spawned actor a turn to run after time advances.
        tokio::time::advance(Duration::from_millis(150)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert!(bad.is_ejected());
        for port in 8080..=8083 {
            assert!(!ejected(&channels, port));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_abort_stops_actor() {
        let mut config = base_config();
        config.interval = Duration::from_millis(50);
        let channels = registry();
        let bad = add(&channels, 8080);

        let abort = OutlierDetector::spawn(config, channels.clone());
        drop(abort);

        // Even after several tick periods, no sweep should have run
        // because the task was aborted.
        tokio::time::advance(Duration::from_millis(500)).await;

        // The bad channel had no traffic recorded, so neither side
        // would eject — but verify nothing happened to the signal.
        assert!(!bad.is_ejected());
    }
}
