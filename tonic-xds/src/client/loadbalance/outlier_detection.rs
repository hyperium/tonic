//! gRFC A50 outlier-detection sweep engine.
//!
//! Owns per-endpoint counters and an ejection state machine. Periodically
//! reads the counters, runs the failure-percentage ejection algorithm,
//! and emits [`EjectionDecision`]s. Knows nothing about the data path:
//! callers feed it RPC outcomes via the lock-free [`EndpointCounters`]
//! handle returned by [`OutlierDetector::add_endpoint`], and consume
//! decisions from a channel returned by [`OutlierDetector::spawn`].
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
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::mpsc;

use crate::client::endpoint::EndpointAddress;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::{FailurePercentageConfig, OutlierDetectionConfig};

/// Default capacity for the channel that delivers [`EjectionDecision`]s
/// from the sweep task to its consumer.
///
/// Sized for several sweeps' worth of decisions on typical clusters —
/// each sweep emits at most `2 * num_endpoints`. At capacity, the sweep
/// task waits on `send` rather than dropping or coalescing decisions:
/// the channel is edge-triggered, so missing or merging events would
/// desynchronize the consumer's view of which endpoints are ejected.
///
/// Override via [`OutlierDetectorOptions::decisions_channel_capacity`].
pub(crate) const DEFAULT_DECISIONS_CHANNEL_CAPACITY: usize = 256;

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

/// Runtime knobs that don't come from the xDS config (`OutlierDetection`
/// proto) — the channel capacity, the RNG, etc. Kept separate from
/// [`OutlierDetectionConfig`] so xDS-derived state stays distinct from
/// host-side runtime tuning.
///
/// New fields can be added without breaking call sites because callers
/// typically construct via `..Default::default()`.
pub(crate) struct OutlierDetectorOptions {
    /// Capacity of the bounded mpsc channel that carries
    /// [`EjectionDecision`]s from the sweep loop to the consumer.
    /// See [`DEFAULT_DECISIONS_CHANNEL_CAPACITY`] for the rationale.
    pub decisions_channel_capacity: usize,
    /// Probability source for the `enforcing_*` rolls. Tests inject a
    /// deterministic [`Rng`]; production uses `fastrand`.
    pub rng: Box<dyn Rng>,
}

impl Default for OutlierDetectorOptions {
    fn default() -> Self {
        Self {
            decisions_channel_capacity: DEFAULT_DECISIONS_CHANNEL_CAPACITY,
            rng: Box::new(FastRandRng),
        }
    }
}

impl std::fmt::Debug for OutlierDetectorOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlierDetectorOptions")
            .field(
                "decisions_channel_capacity",
                &self.decisions_channel_capacity,
            )
            .field("rng", &"<dyn Rng>")
            .finish()
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
    /// Build the detector with default runtime options and spawn its
    /// sweep task on the current Tokio runtime. The sweep runs every
    /// `config.interval` until the returned [`AbortOnDrop`] is dropped.
    pub(crate) fn spawn(
        config: OutlierDetectionConfig,
    ) -> (Arc<Self>, mpsc::Receiver<EjectionDecision>, AbortOnDrop) {
        Self::spawn_with(config, OutlierDetectorOptions::default())
    }

    /// Variant of [`Self::spawn`] that accepts custom runtime options.
    pub(crate) fn spawn_with(
        config: OutlierDetectionConfig,
        options: OutlierDetectorOptions,
    ) -> (Arc<Self>, mpsc::Receiver<EjectionDecision>, AbortOnDrop) {
        let (tx, rx) = mpsc::channel(options.decisions_channel_capacity);
        let detector = Arc::new(Self {
            config,
            state: Mutex::new(HashMap::new()),
            rng: options.rng,
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
    ///
    /// The order of operations follows gRFC A50:
    /// 1. Record the timestamp.
    /// 2. Swap each address's call-counter buckets.
    /// 3. Run the success-rate algorithm if configured.
    /// 4. Run the failure-percentage algorithm if configured.
    /// 5. For each address: decrement the multiplier of non-ejected
    ///    addresses with multiplier > 0, and un-eject ejected addresses
    ///    whose backoff has elapsed.
    pub(crate) fn run_sweep(&self, now: Instant) -> Vec<EjectionDecision> {
        let mut state = self.state.lock().expect("outlier_detector mutex poisoned");

        // Step 2: snapshot every endpoint's counters.
        let mut snapshots: Vec<Candidate> = Vec::with_capacity(state.len());
        for (addr, ep) in state.iter_mut() {
            let (success, failure) = ep.counters.snapshot_and_reset();
            snapshots.push(Candidate {
                addr: addr.clone(),
                success,
                failure,
                total: success + failure,
            });
        }

        // Compute a cap on the number of new ejections this sweep so we
        // don't exceed `max_ejection_percent` of the total. Per A50, the
        // check is performed before each candidate ejection; we model that
        // as a budget that algorithms decrement.
        let total_endpoints = state.len();
        let max_ejections = (total_endpoints as u64
            * u64::from(self.config.max_ejection_percent.get())
            / 100) as usize;
        let already_ejected = state.values().filter(|ep| ep.ejected_at.is_some()).count();
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
            self.run_failure_percentage(fp, &snapshots, &mut budget, &mut to_eject);
        }

        for addr in &to_eject {
            if let Some(ep) = state.get_mut(addr) {
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
        for (addr, ep) in state.iter_mut() {
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
            .collect();
        if qualifying.len() < cfg.minimum_hosts as usize {
            return;
        }

        let threshold = u64::from(cfg.threshold.get());
        for c in qualifying {
            if *budget == 0 {
                break;
            }
            // failure_pct = 100 * failure / total. A50 specifies a strict
            // "greater than" comparison: an address sitting exactly at
            // the threshold is not ejected.
            let failure_pct = 100 * c.failure / c.total;
            if failure_pct > threshold && self.roll(cfg.enforcing_failure_percentage.get()) {
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
///
/// `tx.send().await` is fallible (returns `Err` if the receiver was
/// dropped) and may park briefly when the channel is full — see
/// [`DECISIONS_CHANNEL_CAPACITY`].
async fn sweep_loop(detector: Arc<OutlierDetector>, tx: mpsc::Sender<EjectionDecision>) {
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
            if tx.send(decision).await.is_err() {
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
    fn failure_percentage_at_threshold_does_not_eject() {
        // A50 specifies a strict "greater than" comparison: an address
        // sitting exactly at the threshold should *not* be ejected.
        let detector = detector_no_loop(fp_config(50, 10, 3), FixedRng::boxed(0));
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

    #[test]
    fn multiplier_decrements_even_without_traffic() {
        // A50: a non-ejected address with multiplier > 0 has its
        // multiplier decremented every sweep, regardless of whether it
        // received any RPCs that interval.
        let detector = detector_no_loop(base_config(), FixedRng::boxed(99));
        detector.add_endpoint(addr(8080));
        {
            let mut state = detector.state.lock().unwrap();
            state.get_mut(&addr(8080)).unwrap().ejection_multiplier = 3;
        }
        // No traffic recorded.
        detector.run_sweep(Instant::now());
        let state = detector.state.lock().unwrap();
        assert_eq!(state.get(&addr(8080)).unwrap().ejection_multiplier, 2);
    }

    // ----- Sweep loop -----

    #[tokio::test(start_paused = true)]
    async fn sweep_loop_emits_decisions_on_tick() {
        let mut config = fp_config(50, 10, 3);
        config.interval = Duration::from_millis(100);
        let (detector, mut rx, _abort) = OutlierDetector::spawn_with(
            config,
            OutlierDetectorOptions {
                rng: FixedRng::boxed(99),
                ..Default::default()
            },
        );

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

        // Explicitly advance virtual time past the first sweep tick.
        // `advance` is preferred over `sleep` for paused-time tests — it
        // moves the clock deterministically and yields until pending
        // task wake-ups have been polled, instead of relying on the
        // runtime's auto-advance heuristic for parked tasks.
        tokio::time::advance(Duration::from_millis(150)).await;

        let decision = rx.recv().await.expect("sweep should emit a decision");
        assert_eq!(decision, EjectionDecision::Eject(addr(8084)));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_abort_stops_sweep_loop() {
        let mut config = base_config();
        config.interval = Duration::from_millis(50);
        let (_detector, mut rx, abort) = OutlierDetector::spawn(config);

        // Aborting the JoinHandle wakes the spawned task synchronously;
        // the runtime polls it, the task harness observes the abort,
        // and the task ends — dropping its sender clone. No time
        // advancement is needed: `rx.recv().await` parks briefly, the
        // runtime drives the aborted task to completion, then `recv`
        // returns `None` because the sender is gone.
        drop(abort);
        assert!(rx.recv().await.is_none());
    }
}
