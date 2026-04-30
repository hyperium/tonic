//! gRFC A50 outlier-detection sweep engine.
//!
//! Owns per-endpoint counters and an ejection state machine. Periodically
//! reads the counters, runs the success-rate and failure-percentage
//! ejection algorithms, and emits [`EjectionDecision`]s. Knows nothing
//! about the data path: callers feed it RPC outcomes via the lock-free
//! [`EndpointCounters`] handle returned by [`OutlierDetector::add_endpoint`],
//! and consume decisions from a channel returned by [`OutlierDetector::spawn`].
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::mpsc;

use crate::client::endpoint::EndpointAddress;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::{
    FailurePercentageConfig, OutlierDetectionConfig, SuccessRateConfig,
};

/// Lock-free per-endpoint success/failure counter handle.
///
/// Cloned freely. Callers (typically a request-outcome interceptor)
/// invoke [`record_success`] / [`record_failure`] from the data path.
/// The detector reads and resets the counters during each sweep.
///
/// [`record_success`]: EndpointCounters::record_success
/// [`record_failure`]: EndpointCounters::record_failure
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

    /// Atomically read and zero both counters. Returns `(success, failure)`.
    fn snapshot_and_reset(&self) -> (u64, u64) {
        let s = self.success.swap(0, Ordering::Relaxed);
        let f = self.failure.swap(0, Ordering::Relaxed);
        (s, f)
    }
}

/// A decision emitted by an [`OutlierDetector`] sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Default RNG backed by `fastrand` (already a workspace dep).
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
/// `run_sweep` is pure — it returns a list of [`EjectionDecision`]s
/// rather than sending them. The sweep loop spawned by [`spawn`] owns
/// the channel sender and forwards decisions to the receiver, so
/// dropping the [`AbortOnDrop`] handle ends the loop and closes the
/// receiver. `OutlierDetector` itself holds no I/O resources, which
/// makes algorithm-level tests trivial to write.
///
/// [`spawn`]: OutlierDetector::spawn
pub(crate) struct OutlierDetector {
    config: OutlierDetectionConfig,
    state: Mutex<HashMap<EndpointAddress, EndpointState>>,
    rng: Box<dyn Rng>,
}

impl OutlierDetector {
    /// Build the detector and spawn its sweep task on the current Tokio
    /// runtime. The sweep runs every `config.interval` until the returned
    /// [`AbortOnDrop`] is dropped.
    pub(crate) fn spawn(
        config: OutlierDetectionConfig,
    ) -> (
        Arc<Self>,
        mpsc::UnboundedReceiver<EjectionDecision>,
        AbortOnDrop,
    ) {
        Self::spawn_with_rng(config, Box::new(FastRandRng))
    }

    /// Variant of [`Self::spawn`] that accepts an injected [`Rng`].
    pub(crate) fn spawn_with_rng(
        config: OutlierDetectionConfig,
        rng: Box<dyn Rng>,
    ) -> (
        Arc<Self>,
        mpsc::UnboundedReceiver<EjectionDecision>,
        AbortOnDrop,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let detector = Arc::new(Self {
            config,
            state: Mutex::new(HashMap::new()),
            rng,
        });
        let task = tokio::spawn(sweep_loop(detector.clone(), tx));
        (detector, rx, AbortOnDrop(task))
    }

    /// Register an endpoint and return its lock-free counter handle.
    /// The caller wires this handle into the data-path RPC interceptor so
    /// that completed calls increment success/failure atomics.
    ///
    /// Adding an already-registered address is a no-op and returns the
    /// existing handle (so callers can re-add idempotently).
    pub(crate) fn add_endpoint(&self, addr: EndpointAddress) -> Arc<EndpointCounters> {
        let mut state = self.state.lock().expect("outlier_detector mutex poisoned");
        state
            .entry(addr)
            .or_insert_with(EndpointState::new)
            .counters
            .clone()
    }

    /// Forget a previously-registered endpoint. Drops its counters and
    /// any ejection state. If the endpoint was ejected, no `Uneject`
    /// decision is emitted — the caller is expected to handle the removal
    /// directly (e.g., by dropping its slot in the load balancer).
    pub(crate) fn remove_endpoint(&self, addr: &EndpointAddress) {
        let mut state = self.state.lock().expect("outlier_detector mutex poisoned");
        state.remove(addr);
    }

    /// Run a single sweep at logical time `now` and return the resulting
    /// ejection/un-ejection decisions. Pure — does no I/O. The sweep loop
    /// invokes this on each interval tick and forwards the decisions on
    /// the channel; tests call it directly.
    pub(crate) fn run_sweep(&self, now: Instant) -> Vec<EjectionDecision> {
        let mut state = self.state.lock().expect("outlier_detector mutex poisoned");

        // Snapshot per-endpoint stats and update ejection-time multiplier
        // bookkeeping. A50: for each endpoint that received traffic and is
        // not currently ejected, decrement the multiplier toward zero.
        let mut snapshots: Vec<(EndpointAddress, u64, u64)> = Vec::with_capacity(state.len());
        for (addr, ep) in state.iter_mut() {
            let (success, failure) = ep.counters.snapshot_and_reset();
            let total = success + failure;
            if ep.ejected_at.is_none() && total > 0 {
                ep.ejection_multiplier = ep.ejection_multiplier.saturating_sub(1);
            }
            snapshots.push((addr.clone(), success, failure));
        }

        // Un-eject endpoints whose backoff has elapsed. A50:
        //   actual_duration = min(base * multiplier, max(base, max_ejection_time))
        let cap = self
            .config
            .base_ejection_time
            .max(self.config.max_ejection_time);
        let mut to_uneject: Vec<EndpointAddress> = Vec::new();
        for (addr, ep) in state.iter_mut() {
            if let Some(at) = ep.ejected_at
                && let Some(scaled) = self
                    .config
                    .base_ejection_time
                    .checked_mul(ep.ejection_multiplier)
                && now.duration_since(at) >= scaled.min(cap)
            {
                ep.ejected_at = None;
                to_uneject.push(addr.clone());
            }
        }

        // Build candidate list (non-ejected endpoints) once for both
        // algorithms. A50 wants both algorithms to share the snapshot.
        // Note: we only build the rate slice; per-algorithm filters
        // (request_volume, minimum_hosts) are applied below.
        let candidates: Vec<Candidate> = snapshots
            .iter()
            .filter_map(|(addr, success, failure)| {
                let total = success + failure;
                let ep = state.get(addr)?;
                if ep.ejected_at.is_some() {
                    return None;
                }
                Some(Candidate {
                    addr: addr.clone(),
                    success: *success,
                    failure: *failure,
                    total,
                })
            })
            .collect();

        // Compute the cap on currently-ejected endpoints. A50:
        //   if ejected_count >= max_ejection_percent of total, stop ejecting.
        // We compute the cap once and decrement the available budget as
        // each algorithm ejects.
        let total_endpoints = state.len();
        let max_ejections = (total_endpoints as u64
            * u64::from(self.config.max_ejection_percent.get())
            / 100) as usize;
        let already_ejected = state.values().filter(|ep| ep.ejected_at.is_some()).count();
        let mut budget = max_ejections.saturating_sub(already_ejected);

        let mut to_eject: Vec<EndpointAddress> = Vec::new();

        if let Some(sr) = self.config.success_rate.as_ref() {
            self.run_success_rate(sr, &candidates, &mut budget, &mut to_eject);
        }
        if let Some(fp) = self.config.failure_percentage.as_ref() {
            self.run_failure_percentage(fp, &candidates, &mut budget, &mut to_eject);
        }

        for addr in &to_eject {
            if let Some(ep) = state.get_mut(addr) {
                ep.ejected_at = Some(now);
                ep.ejection_multiplier = ep.ejection_multiplier.saturating_add(1);
            }
        }

        drop(state);

        let mut decisions = Vec::with_capacity(to_uneject.len() + to_eject.len());
        for addr in to_uneject {
            decisions.push(EjectionDecision::Uneject(addr));
        }
        for addr in to_eject {
            decisions.push(EjectionDecision::Eject(addr));
        }
        decisions
    }

    /// A50 success-rate algorithm.
    fn run_success_rate(
        &self,
        cfg: &SuccessRateConfig,
        all: &[Candidate],
        budget: &mut usize,
        out: &mut Vec<EndpointAddress>,
    ) {
        // Filter to candidates with enough traffic.
        let qualifying: Vec<&Candidate> = all
            .iter()
            .filter(|c| c.total >= u64::from(cfg.request_volume))
            .collect();
        if qualifying.len() < cfg.minimum_hosts as usize {
            return;
        }

        // success_rate = success / total (in [0.0, 1.0]).
        let rates: Vec<f64> = qualifying
            .iter()
            .map(|c| c.success as f64 / c.total as f64)
            .collect();
        let n = rates.len() as f64;
        let mean = rates.iter().sum::<f64>() / n;
        let variance = rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
        let stdev = variance.sqrt();

        // threshold = mean - stdev * (stdev_factor / 1000)
        let factor = f64::from(cfg.stdev_factor) / 1000.0;
        let threshold = mean - stdev * factor;

        for (c, rate) in qualifying.iter().zip(rates.iter()) {
            if *budget == 0 {
                break;
            }
            if *rate < threshold && self.roll(cfg.enforcing_success_rate.get()) {
                out.push(c.addr.clone());
                *budget -= 1;
            }
        }
    }

    /// A50 failure-percentage algorithm.
    fn run_failure_percentage(
        &self,
        cfg: &FailurePercentageConfig,
        all: &[Candidate],
        budget: &mut usize,
        out: &mut Vec<EndpointAddress>,
    ) {
        let qualifying: Vec<&Candidate> = all
            .iter()
            .filter(|c| c.total >= u64::from(cfg.request_volume))
            .filter(|c| !out.contains(&c.addr)) // skip endpoints already ejected this sweep
            .collect();
        if qualifying.len() < cfg.minimum_hosts as usize {
            return;
        }

        let threshold = u64::from(cfg.threshold.get());
        for c in qualifying {
            if *budget == 0 {
                break;
            }
            // failure_pct = 100 * failure / total
            let failure_pct = 100 * c.failure / c.total;
            if failure_pct >= threshold && self.roll(cfg.enforcing_failure_percentage.get()) {
                out.push(c.addr.clone());
                *budget -= 1;
            }
        }
    }

    /// Return true with probability `pct / 100` (clamped at 100 ⇒ always).
    fn roll(&self, pct: u8) -> bool {
        if pct >= 100 {
            return true;
        }
        if pct == 0 {
            return false;
        }
        self.rng.pct_roll() < u32::from(pct)
    }
}

/// Cached per-endpoint snapshot used during a sweep.
struct Candidate {
    addr: EndpointAddress,
    success: u64,
    failure: u64,
    total: u64,
}

/// Background task: runs `detector.run_sweep` on each interval tick and
/// forwards each decision on the channel. The task ends (and `tx` is
/// dropped, closing the receiver) when [`AbortOnDrop`] is dropped or
/// when the receiver itself is dropped.
async fn sweep_loop(detector: Arc<OutlierDetector>, tx: mpsc::UnboundedSender<EjectionDecision>) {
    let mut ticker = tokio::time::interval(detector.config.interval);
    // Skip missed ticks rather than burst-catching up — the goal is
    // periodic observation, not making up for paused time.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first tick fires immediately; consume it so the first real
    // sweep is `interval` after spawn (matches A50 semantics).
    ticker.tick().await;

    loop {
        ticker.tick().await;
        for decision in detector.run_sweep(Instant::now()) {
            if tx.send(decision).is_err() {
                // Receiver gone — nobody is listening.
                return;
            }
        }
    }
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

    /// Build a detector with no sweep loop running. Tests drive
    /// `run_sweep` directly and inspect the returned decisions.
    fn detector_no_loop(config: OutlierDetectionConfig, rng: Box<dyn Rng>) -> Arc<OutlierDetector> {
        Arc::new(OutlierDetector {
            config,
            state: Mutex::new(HashMap::new()),
            rng,
        })
    }

    /// Sort a decision list deterministically so equality checks can rely
    /// on a canonical order without coupling to `HashMap` iteration order.
    fn sort(mut ds: Vec<EjectionDecision>) -> Vec<EjectionDecision> {
        ds.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        ds
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
        let detector = detector_no_loop(base_config(), FixedRng::boxed(99));
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
        let detector = detector_no_loop(base_config(), FixedRng::boxed(99));
        detector.add_endpoint(addr(8080));
        detector.remove_endpoint(&addr(8080));
        assert!(detector.state.lock().unwrap().is_empty());
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
        let detector = detector_no_loop(fp_config(50, 10, 3), FixedRng::boxed(99));
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
        let detector = detector_no_loop(fp_config(50, 10, 3), FixedRng::boxed(99));
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
    fn minimum_hosts_gates_failure_percentage() {
        let detector = detector_no_loop(fp_config(50, 10, 5), FixedRng::boxed(99));
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
        let detector = detector_no_loop(fp_config(50, 100, 3), FixedRng::boxed(99));
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
        let detector = detector_no_loop(config, FixedRng::boxed(0));
        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }
        assert!(detector.run_sweep(Instant::now()).is_empty());
    }

    // ----- Success-rate algorithm -----

    fn sr_config(
        stdev_factor: u32,
        request_volume: u32,
        minimum_hosts: u32,
    ) -> OutlierDetectionConfig {
        let mut c = base_config();
        c.success_rate = Some(SuccessRateConfig {
            stdev_factor,
            enforcing_success_rate: pct(100),
            minimum_hosts,
            request_volume,
        });
        c
    }

    #[test]
    fn success_rate_ejects_outlier_below_threshold() {
        let detector = detector_no_loop(sr_config(1900, 10, 5), FixedRng::boxed(99));
        // 4 endpoints at 99% success, 1 at 50% — outlier.
        for port in 8080..=8083 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..99 {
                h.record_success();
            }
            h.record_failure();
        }
        let bad = detector.add_endpoint(addr(8084));
        for _ in 0..50 {
            bad.record_success();
        }
        for _ in 0..50 {
            bad.record_failure();
        }
        assert_eq!(
            detector.run_sweep(Instant::now()),
            vec![EjectionDecision::Eject(addr(8084))],
        );
    }

    #[test]
    fn success_rate_no_ejection_when_all_uniform() {
        let detector = detector_no_loop(sr_config(1900, 10, 5), FixedRng::boxed(99));
        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..95 {
                h.record_success();
            }
            for _ in 0..5 {
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
        let detector = detector_no_loop(config, FixedRng::boxed(99));

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
        let detector = detector_no_loop(config, FixedRng::boxed(99));

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

        // Sweep 2 at t0+10: same-sweep un-eject + re-eject.
        // Multiplier stays 1 through un-eject, then 1 → 2 on re-eject.
        assert_eq!(
            detector.run_sweep(t0 + Duration::from_secs(10)),
            vec![
                EjectionDecision::Uneject(bad.clone()),
                EjectionDecision::Eject(bad.clone()),
            ],
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
        let detector = detector_no_loop(config, FixedRng::boxed(99));

        for port in 8080..=8084 {
            detector.add_endpoint(addr(port));
        }
        let t0 = Instant::now();
        // Force multiplier=10 directly.
        {
            let mut state = detector.state.lock().unwrap();
            let ep = state.get_mut(&addr(8084)).unwrap();
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
        let detector = detector_no_loop(config, FixedRng::boxed(99));

        for port in 8080..=8084 {
            let h = detector.add_endpoint(addr(port));
            for _ in 0..100 {
                h.record_failure();
            }
        }
        let decisions = sort(detector.run_sweep(Instant::now()));
        let ejects = decisions
            .iter()
            .filter(|d| matches!(d, EjectionDecision::Eject(_)))
            .count();
        assert_eq!(ejects, 1, "max_ejection_percent=20% of 5 hosts ⇒ 1");
    }

    #[test]
    fn multiplier_decrements_on_healthy_interval() {
        let detector = detector_no_loop(base_config(), FixedRng::boxed(99));
        let h = detector.add_endpoint(addr(8080));
        // Force multiplier to 3 without ejecting.
        {
            let mut state = detector.state.lock().unwrap();
            state.get_mut(&addr(8080)).unwrap().ejection_multiplier = 3;
        }
        // Healthy interval (some traffic, no ejection).
        h.record_success();
        detector.run_sweep(Instant::now());
        let state = detector.state.lock().unwrap();
        assert_eq!(state.get(&addr(8080)).unwrap().ejection_multiplier, 2);
    }

    // ----- Sweep loop -----

    #[tokio::test(start_paused = true)]
    async fn sweep_loop_emits_decisions_on_tick() {
        let mut config = fp_config(50, 10, 3);
        config.interval = Duration::from_millis(100);
        let (detector, mut rx, _abort) =
            OutlierDetector::spawn_with_rng(config, FixedRng::boxed(99));

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

        // Advance just past the first sweep tick.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let decision = rx.recv().await.expect("sweep should emit a decision");
        assert_eq!(decision, EjectionDecision::Eject(addr(8084)));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_abort_stops_sweep_loop() {
        let mut config = base_config();
        config.interval = Duration::from_millis(50);
        let (_detector, mut rx, abort) = OutlierDetector::spawn(config);

        // Drop the AbortOnDrop; the loop must terminate.
        drop(abort);
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Sender should be dropped along with the task; recv returns None.
        assert!(rx.recv().await.is_none());
    }
}
