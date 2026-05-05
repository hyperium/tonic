//! gRFC A50 outlier-detection sweep engine.
//!
//! Owns per-endpoint counters and an ejection state machine. Runs the
//! failure-percentage ejection algorithm on demand and returns the
//! resulting [`EjectionDecision`]s. Knows nothing about the data path:
//! callers feed it RPC outcomes via the lock-free [`EndpointCounters`]
//! handle returned by [`OutlierDetector::add_endpoint`], and pump the
//! sweep by calling [`OutlierDetector::maybe_run_sweep`] from their own
//! event loop (typically the load balancer's `poll_ready`). The wall
//! clock supplied to `maybe_run_sweep` decides when each sweep actually
//! runs — at most once per `config.interval`.
//!
//! Only the **failure-percentage** algorithm is implemented in this
//! module. The success-rate algorithm — which adds float-math (mean
//! and standard deviation across the qualifying hosts) — lands in a
//! follow-up PR. If [`OutlierDetectionConfig::success_rate`] is set,
//! it is currently ignored.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md
//! [`OutlierDetectionConfig::success_rate`]: crate::xds::resource::outlier_detection::OutlierDetectionConfig::success_rate

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::client::endpoint::EndpointAddress;
use crate::xds::resource::outlier_detection::{FailurePercentageConfig, OutlierDetectionConfig};

/// Lock-free success/failure counter for one endpoint. The data path
/// records RPC outcomes via `record_success` / `record_failure`; the
/// sweep snapshots and resets atomically.
///
/// Counts are packed into a single `AtomicU64` (high 32 bits:
/// successes, low 32 bits: failures), so each record is one `fetch_add`
/// and a snapshot is one `swap(0)`. Each counter is capped at
/// `u32::MAX` per sweep interval; exceeding that carries into the
/// other counter's bits but is unreachable for realistic workloads.
#[derive(Debug, Default)]
pub(crate) struct EndpointCounters {
    /// High 32 bits: successes since last sweep.
    /// Low 32 bits: failures since last sweep.
    packed: AtomicU64,
}

/// Increment to apply to [`EndpointCounters::packed`] for one success.
const SUCCESS_INC: u64 = 1 << 32;
/// Increment to apply to [`EndpointCounters::packed`] for one failure.
const FAILURE_INC: u64 = 1;
/// Mask for the failure half of the packed counter.
const FAILURE_MASK: u64 = 0xFFFF_FFFF;

impl EndpointCounters {
    pub(crate) fn record_success(&self) {
        self.packed.fetch_add(SUCCESS_INC, Ordering::Relaxed);
    }

    pub(crate) fn record_failure(&self) {
        self.packed.fetch_add(FAILURE_INC, Ordering::Relaxed);
    }

    /// Atomically read and zero both counters. Returns `(success, failure)`.
    fn snapshot_and_reset(&self) -> (u64, u64) {
        let v = self.packed.swap(0, Ordering::Relaxed);
        (v >> 32, v & FAILURE_MASK)
    }
}

/// A decision emitted by an [`OutlierDetector`] sweep.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EjectionDecision {
    /// Eject this endpoint from the load-balancing pool. The caller
    /// should keep its underlying connection alive (A50 requires
    /// preserving connections across ejection).
    Eject(EndpointAddress),
    /// Restore a previously-ejected endpoint to the pool.
    Uneject(EndpointAddress),
}

/// Probability source for `enforcing_*` rolls. Abstracted so tests can
/// inject deterministic outcomes.
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

/// Per-endpoint state held inside the detector.
struct EndpointState {
    counters: Arc<EndpointCounters>,
    /// Number of times this endpoint has been ejected. Grows on each
    /// re-ejection and decays on each healthy interval.
    ejection_multiplier: u32,
    /// `Some(at)` when currently ejected; `None` otherwise.
    ejected_at: Option<Instant>,
}

impl EndpointState {
    fn new() -> Self {
        Self {
            counters: Arc::new(EndpointCounters::default()),
            ejection_multiplier: 0,
            ejected_at: None,
        }
    }
}

/// gRFC A50 outlier detector.
///
/// State is owned (no `Mutex`, no `Arc`): the consumer holds the
/// detector by `&mut` and calls [`Self::maybe_run_sweep`] from its own
/// event loop, typically the load balancer's `poll_ready`. The wall
/// clock argument decides when each sweep actually runs — at most once
/// per `config.interval`.
pub(crate) struct OutlierDetector {
    config: OutlierDetectionConfig,
    state: HashMap<EndpointAddress, EndpointState>,
    /// Wall-clock time of the last sweep that actually ran. `None`
    /// before the first sweep, so the first call to `maybe_run_sweep`
    /// always runs.
    last_sweep_at: Option<Instant>,
    rng: Box<dyn Rng>,
}

impl OutlierDetector {
    /// Build the detector with the default RNG (`fastrand`).
    pub(crate) fn new(config: OutlierDetectionConfig) -> Self {
        Self::with_rng(config, Box::new(FastRandRng))
    }

    /// Build the detector with an injected [`Rng`]. Tests use this to
    /// pin the `enforcing_*` rolls.
    pub(crate) fn with_rng(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> Self {
        Self {
            config,
            state: HashMap::new(),
            last_sweep_at: None,
            rng,
        }
    }

    /// Register an endpoint and return its lock-free counter handle.
    /// The caller wires this handle into the data-path RPC interceptor so
    /// that completed calls increment success/failure atomics.
    ///
    /// Adding an already-registered address is a no-op and returns the
    /// existing handle (so callers can re-add idempotently).
    pub(crate) fn add_endpoint(&mut self, addr: EndpointAddress) -> Arc<EndpointCounters> {
        self.state
            .entry(addr)
            .or_insert_with(EndpointState::new)
            .counters
            .clone()
    }

    /// Forget a previously-registered endpoint. Drops its counters and
    /// any ejection state. If the endpoint was ejected, no `Uneject`
    /// decision is emitted — the caller is expected to handle the removal
    /// directly (e.g., by dropping its slot in the load balancer).
    pub(crate) fn remove_endpoint(&mut self, addr: &EndpointAddress) {
        self.state.remove(addr);
    }

    /// Run a sweep at logical time `now` if at least `config.interval`
    /// has elapsed since the last sweep, returning the resulting
    /// ejection / un-ejection decisions. Otherwise returns an empty
    /// vector and leaves the detector state untouched.
    ///
    /// The first call after construction always runs a sweep
    /// (`last_sweep_at` starts as `None`).
    pub(crate) fn maybe_run_sweep(&mut self, now: Instant) -> Vec<EjectionDecision> {
        if let Some(last) = self.last_sweep_at
            && now.duration_since(last) < self.config.interval
        {
            return Vec::new();
        }
        self.last_sweep_at = Some(now);
        self.run_sweep(now)
    }

    /// Unconditionally run one sweep at logical time `now` and return the
    /// resulting decisions. Used by [`Self::maybe_run_sweep`] and by tests
    /// that want to drive sweeps without modeling the interval gate.
    ///
    /// The order of operations follows gRFC A50:
    /// 1. Record the timestamp.
    /// 2. Swap each address's call-counter buckets.
    /// 3. Run the success-rate algorithm if configured.
    /// 4. Run the failure-percentage algorithm if configured.
    /// 5. For each address: decrement the multiplier of non-ejected
    ///    addresses with multiplier > 0, and un-eject ejected addresses
    ///    whose backoff has elapsed.
    pub(crate) fn run_sweep(&mut self, now: Instant) -> Vec<EjectionDecision> {
        // Step 2: snapshot every endpoint's counters.
        let mut snapshots: Vec<Candidate> = Vec::with_capacity(self.state.len());
        for (addr, ep) in self.state.iter_mut() {
            let (success, failure) = ep.counters.snapshot_and_reset();
            snapshots.push(Candidate {
                addr: addr.clone(),
                success,
                failure,
                total: success + failure,
                already_ejected: ep.ejected_at.is_some(),
            });
        }

        // Compute a cap on the number of new ejections this sweep so we
        // don't exceed `max_ejection_percent` of the total. Per A50, the
        // check is performed before each candidate ejection; we model that
        // as a budget that algorithms decrement.
        let total_endpoints = self.state.len();
        let max_ejections = (total_endpoints as u64
            * u64::from(self.config.max_ejection_percent.get())
            / 100) as usize;
        let already_ejected = self
            .state
            .values()
            .filter(|ep| ep.ejected_at.is_some())
            .count();
        let mut budget = max_ejections.saturating_sub(already_ejected);

        // Steps 3 & 4: run the algorithms on the snapshot. Hosts that are
        // currently ejected naturally fail the `request_volume` gate
        // because they receive no traffic in production, so iterating
        // every address (per spec) and ejected-only candidates produce
        // the same outcome on real workloads.
        //
        // Step 3 (`success_rate_ejection`) is intentionally not yet
        // dispatched in this PR; it lands in a follow-up.
        let mut to_eject: Vec<EndpointAddress> = Vec::new();

        if let Some(fp) = self.config.failure_percentage.as_ref() {
            run_failure_percentage(fp, &snapshots, &mut budget, &mut to_eject, &*self.rng);
        }

        for addr in &to_eject {
            if let Some(ep) = self.state.get_mut(addr) {
                ep.ejected_at = Some(now);
                ep.ejection_multiplier = ep.ejection_multiplier.saturating_add(1);
            }
        }

        // Step 5: decrement multipliers for non-ejected addresses, and
        // un-eject any ejected addresses whose backoff has elapsed. This
        // runs *after* re-ejection, so a same-sweep re-ejection updates
        // `ejected_at` to `now` and the un-eject check sees zero elapsed
        // time — no spurious uneject decision is emitted.
        let cap = self
            .config
            .base_ejection_time
            .max(self.config.max_ejection_time);
        let mut to_uneject: Vec<EndpointAddress> = Vec::new();
        for (addr, ep) in self.state.iter_mut() {
            if let Some(at) = ep.ejected_at {
                if let Some(scaled) = self
                    .config
                    .base_ejection_time
                    .checked_mul(ep.ejection_multiplier)
                    && now.duration_since(at) >= scaled.min(cap)
                {
                    ep.ejected_at = None;
                    to_uneject.push(addr.clone());
                }
            } else if ep.ejection_multiplier > 0 {
                ep.ejection_multiplier -= 1;
            }
        }

        let mut decisions = Vec::with_capacity(to_uneject.len() + to_eject.len());
        for addr in to_uneject {
            decisions.push(EjectionDecision::Uneject(addr));
        }
        for addr in to_eject {
            decisions.push(EjectionDecision::Eject(addr));
        }
        decisions
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
            // Only NEW ejections consume a budget slot; re-ejecting
            // an already-ejected address only refreshes its
            // timestamp and multiplier, leaving the count of
            // currently-ejected addresses unchanged.
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
    /// Whether this address was already ejected at the start of the sweep.
    /// "Re-ejecting" an already-ejected address only refreshes its
    /// ejection timestamp and bumps the multiplier; it does not change
    /// the count of currently-ejected addresses, so it must not consume
    /// a `max_ejection_percent` budget slot.
    already_ejected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::outlier_detection::Percentage;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    // ----- Fixtures -----

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("10.0.0.1", port)
    }

    fn pct(v: u32) -> Percentage {
        Percentage::new(v).unwrap()
    }

    /// Base config with both algorithms disabled; tests opt in.
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

    /// Deterministic RNG: `pct_roll()` returns a fixed value, configurable.
    struct FixedRng(AtomicU32);

    impl FixedRng {
        fn new(value: u32) -> Self {
            Self(AtomicU32::new(value))
        }
        fn boxed(value: u32) -> Box<dyn Rng> {
            Box::new(Self::new(value))
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

    // ----- EndpointCounters -----

    #[test]
    fn counters_record_and_reset() {
        let c = EndpointCounters::default();
        c.record_success();
        c.record_success();
        c.record_failure();
        assert_eq!(c.snapshot_and_reset(), (2, 1));
        assert_eq!(c.snapshot_and_reset(), (0, 0));
    }

    // ----- add_endpoint / remove_endpoint -----

    #[test]
    fn add_endpoint_returns_shared_counter() {
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        let h1 = detector.add_endpoint(addr(8080));
        let h2 = detector.add_endpoint(addr(8080));
        assert!(
            Arc::ptr_eq(&h1, &h2),
            "second add should return same handle"
        );
        h1.record_success();
        assert_eq!(h2.snapshot_and_reset(), (1, 0));
    }

    #[test]
    fn remove_endpoint_drops_state() {
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        detector.add_endpoint(addr(8080));
        detector.remove_endpoint(&addr(8080));
        assert!(detector.state.is_empty());
    }

    // ----- Failure-percentage algorithm -----

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

    #[test]
    fn failure_percentage_ejects_above_threshold() {
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        // 4 healthy endpoints + 1 bad one.
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        let bad = detector.add_endpoint(addr(8084));
        for _ in 0..90 {
            bad.record_failure();
        }
        for _ in 0..10 {
            bad.record_success();
        }

        let decisions = detector.run_sweep(Instant::now());
        assert_eq!(decisions, vec![EjectionDecision::Eject(addr(8084))]);
    }

    #[test]
    fn failure_percentage_skips_below_threshold() {
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            // 30% failure → below threshold of 50%.
            for _ in 0..70 {
                h.record_success();
            }
            for _ in 0..30 {
                h.record_failure();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    #[test]
    fn failure_percentage_at_threshold_does_not_eject() {
        // A50 specifies a strict "greater than" comparison: an address
        // sitting exactly at the threshold should *not* be ejected.
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(0));
        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            // Exactly 50% failure rate — equal to the threshold.
            for _ in 0..50 {
                h.record_success();
            }
            for _ in 0..50 {
                h.record_failure();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    #[test]
    fn minimum_hosts_gates_failure_percentage() {
        let mut detector = detector_with_rng(fp_config(50, 10, 5), FixedRng::boxed(99));
        // Only 2 hosts have request_volume ≥ 10; minimum_hosts is 5 ⇒ skip.
        for port in 8080..=8081 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    #[test]
    fn request_volume_filters_low_traffic_endpoints() {
        let mut detector = detector_with_rng(fp_config(50, 100, 3), FixedRng::boxed(99));
        // Bad endpoint, but only 5 requests — below request_volume=100.
        let bad = detector.add_endpoint(addr(8080));
        for _ in 0..5 {
            bad.record_failure();
        }
        for port in 8081..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..200 {
                h.record_success();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    #[test]
    fn enforcement_zero_percent_never_ejects() {
        let mut config = fp_config(50, 10, 3);
        config
            .failure_percentage
            .as_mut()
            .unwrap()
            .enforcing_failure_percentage = pct(0);
        // Roll = 0 wouldn't trigger anyway since `roll(0)` short-circuits;
        // pin the RNG to 0 just to be explicit.
        let mut detector = detector_with_rng(config, FixedRng::boxed(0));
        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    // ----- Ejection multiplier / un-ejection -----

    #[test]
    fn unejects_after_base_time() {
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));

        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            if port == 8084 {
                for _ in 0..100 {
                    h.record_failure();
                }
            } else {
                for _ in 0..100 {
                    h.record_success();
                }
            }
        }

        let t0 = Instant::now();
        assert_eq!(
            detector.run_sweep(t0),
            vec![EjectionDecision::Eject(addr(8084))],
        );

        // Still ejected just before base_ejection_time elapses.
        assert!(detector.run_sweep(t0 + Duration::from_secs(9)).is_empty());

        // Un-eject after `base * multiplier(=1)` = 10s.
        assert_eq!(
            detector.run_sweep(t0 + Duration::from_secs(10)),
            vec![EjectionDecision::Uneject(addr(8084))],
        );
    }

    #[test]
    fn re_ejection_doubles_duration() {
        // The multiplier doubles only when un-ejection and re-ejection
        // happen in the *same* sweep — at that point the multiplier-
        // decrement step has skipped the (still-ejected-at-start)
        // endpoint, so re-ejection increments it from 1 to 2.
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));

        let bad = addr(8084);
        let bad_h = detector.add_endpoint(bad.clone());
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        for _ in 0..100 {
            bad_h.record_failure();
        }

        // Sweep 1: eject. Multiplier 0 → 1.
        let t0 = Instant::now();
        assert_eq!(
            detector.run_sweep(t0),
            vec![EjectionDecision::Eject(bad.clone())],
        );

        // Re-record stats so sweep 2's snapshot has volume to evaluate.
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        for _ in 0..100 {
            bad_h.record_failure();
        }

        // Sweep 2 at t0+10: re-ejection happens before the un-eject
        // housekeeping step (per A50 ordering), so `ejected_at` is
        // refreshed to `now` and the un-eject check sees zero elapsed
        // time. Only an Eject decision is emitted; the multiplier moves
        // 1 → 2.
        assert_eq!(
            detector.run_sweep(t0 + Duration::from_secs(10)),
            vec![EjectionDecision::Eject(bad.clone())],
        );

        // Re-ejection started at t0+10 with multiplier=2 → duration 20s.
        // Still ejected 19s later (29s after t0).
        assert!(detector.run_sweep(t0 + Duration::from_secs(29)).is_empty());

        // Un-ejects at the 20s mark (30s after t0).
        assert_eq!(
            detector.run_sweep(t0 + Duration::from_secs(30)),
            vec![EjectionDecision::Uneject(bad)],
        );
    }

    #[test]
    fn ejection_capped_by_max_ejection_time() {
        // base=10s, max=15s, multiplier=10 → cap at 15s rather than 100s.
        let mut config = fp_config(50, 10, 3);
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(15);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));

        for port in 8080..=8084 {
            detector.add_endpoint(addr(port));
        }
        let t0 = Instant::now();
        // Force multiplier=10 directly.
        {
            let ep = detector.state.get_mut(&addr(8084)).unwrap();
            ep.ejection_multiplier = 10;
            ep.ejected_at = Some(t0);
        }
        // After base*multiplier (= 100s) the cap (= 15s) has long passed,
        // so a sweep at 16s should un-eject.
        let decisions = detector.run_sweep(t0 + Duration::from_secs(16));
        assert_eq!(decisions, vec![EjectionDecision::Uneject(addr(8084))]);
    }

    #[test]
    fn max_ejection_percent_caps_concurrent_ejections() {
        // 5 hosts, all bad, but max_ejection_percent=20 ⇒ at most 1 ejected.
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(20);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));

        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }
        let mut decisions = detector.run_sweep(Instant::now());
        decisions.sort();
        let ejects = decisions
            .iter()
            .filter(|d| matches!(d, EjectionDecision::Eject(_)))
            .count();
        assert_eq!(ejects, 1, "max_ejection_percent=20% of 5 hosts ⇒ 1");
    }

    #[test]
    fn already_ejected_re_ejection_does_not_consume_budget() {
        // 5 hosts: one already ejected (with stats from in-flight RPCs
        // accumulated during its backoff), four newly bad. Cap permits
        // 3 concurrently ejected hosts (60% of 5), with 1 already taken
        // by the pre-ejected host — so 2 new ejections remain in budget.
        //
        // This test would fail before the fix that excludes re-ejections
        // from budget accounting: the algorithm would "re-eject" the
        // already-ejected host (consuming the second slot), leaving only
        // 1 new ejection from the four bad hosts.
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(60);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));

        // Pre-eject host 8080 directly and give it bad in-flight stats.
        let already_bad = detector.add_endpoint(addr(8080));
        for _ in 0..100 {
            already_bad.record_failure();
        }
        {
            let ep = detector.state.get_mut(&addr(8080)).unwrap();
            ep.ejected_at = Some(Instant::now());
            ep.ejection_multiplier = 1;
        }

        // Four more bad hosts.
        for port in 8081..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }

        let mut decisions = detector.run_sweep(Instant::now());
        decisions.sort();
        let new_ejects = decisions
            .iter()
            .filter(|d| matches!(d, EjectionDecision::Eject(a) if *a != addr(8080)))
            .count();
        assert_eq!(new_ejects, 2, "expected 2 new ejections under the cap");
    }

    #[test]
    fn multiplier_decrements_on_healthy_interval() {
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        let h = detector.add_endpoint(addr(8080));
        // Force multiplier to 3 without ejecting.
        detector
            .state
            .get_mut(&addr(8080))
            .unwrap()
            .ejection_multiplier = 3;
        // Healthy interval (some traffic, no ejection).
        h.record_success();
        detector.run_sweep(Instant::now());
        assert_eq!(
            detector.state.get(&addr(8080)).unwrap().ejection_multiplier,
            2,
        );
    }

    #[test]
    fn multiplier_decrements_even_without_traffic() {
        // A50: a non-ejected address with multiplier > 0 has its
        // multiplier decremented every sweep, regardless of whether it
        // received any RPCs that interval.
        let mut detector = detector_with_rng(base_config(), FixedRng::boxed(99));
        detector.add_endpoint(addr(8080));
        detector
            .state
            .get_mut(&addr(8080))
            .unwrap()
            .ejection_multiplier = 3;
        // No traffic recorded.
        detector.run_sweep(Instant::now());
        assert_eq!(
            detector.state.get(&addr(8080)).unwrap().ejection_multiplier,
            2,
        );
    }

    // ----- maybe_run_sweep gating -----

    #[test]
    fn maybe_run_sweep_runs_on_first_call() {
        // `last_sweep_at` starts as `None`, so the first call always
        // sweeps regardless of the wall clock argument.
        let mut detector = detector_with_rng(fp_config(50, 10, 3), FixedRng::boxed(99));
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        let bad = detector.add_endpoint(addr(8084));
        for _ in 0..100 {
            bad.record_failure();
        }
        let decisions = detector.maybe_run_sweep(Instant::now());
        assert_eq!(decisions, vec![EjectionDecision::Eject(addr(8084))]);
    }

    #[test]
    fn maybe_run_sweep_skips_when_interval_not_elapsed() {
        let mut config = fp_config(50, 10, 3);
        config.interval = Duration::from_secs(10);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        let bad = detector.add_endpoint(addr(8084));
        for _ in 0..100 {
            bad.record_failure();
        }

        // First call always runs.
        let t0 = Instant::now();
        assert_eq!(
            detector.maybe_run_sweep(t0),
            vec![EjectionDecision::Eject(addr(8084))],
        );

        // Re-arm with bad stats; second call <interval after the first
        // is a no-op even though the snapshot would otherwise eject.
        let bad_h = detector.add_endpoint(addr(8084));
        for _ in 0..100 {
            bad_h.record_failure();
        }
        assert!(
            detector
                .maybe_run_sweep(t0 + Duration::from_secs(9))
                .is_empty(),
        );
    }

    #[test]
    fn maybe_run_sweep_runs_after_interval_elapsed() {
        let mut config = fp_config(50, 10, 3);
        config.interval = Duration::from_secs(10);
        let mut detector = detector_with_rng(config, FixedRng::boxed(99));
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        // First call sets the high-water mark with no decisions.
        let t0 = Instant::now();
        assert!(detector.maybe_run_sweep(t0).is_empty());

        // After interval, traffic that arrived in between is observed.
        let bad = detector.add_endpoint(addr(8084));
        for _ in 0..100 {
            bad.record_failure();
        }
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_success();
            }
        }
        assert_eq!(
            detector.maybe_run_sweep(t0 + Duration::from_secs(10)),
            vec![EjectionDecision::Eject(addr(8084))],
        );
    }
}
