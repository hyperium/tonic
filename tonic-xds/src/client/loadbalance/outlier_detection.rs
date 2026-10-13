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

//! [gRFC A50] outlier detection.
//!
//! Work is split across three sites:
//!
//! - **Data path** (`ReadyChannel::record_outcome`): runs inline per
//!   RPC. Updates per-channel counters only; ejection decisions are
//!   deferred to the sweep.
//! - **Load balancer**: drains the ejected-set snapshot broadcast by
//!   the sweep on a `watch` channel, consumes the matching
//!   [`ReadyChannel`] via [`ReadyChannel::eject`], and tracks the
//!   resulting [`EjectedChannel`] in a `KeyedFutures`. Each ejected
//!   channel's sleep fires at `base × multiplier` (capped by
//!   `max_ejection_time`); the LB then routes the resolved
//!   [`UnejectedChannel`] back into the ready set.
//! - **Housekeeping actor** ([`spawn_actor`]): on each
//!   `config.interval` tick, runs the failure-percentage algorithm
//!   over a snapshot of counters, ejects qualifying channels, resets
//!   counters, and decrements multipliers for non-ejected channels.
//!   When the ejected-set membership changes, broadcasts a fresh
//!   snapshot on the `watch` channel; quiet ticks skip the broadcast
//!   via an O(1) version compare.
//!
//! Only the failure-percentage algorithm is implemented; success-rate
//! (cross-endpoint mean/stdev) is left to a follow-up.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md
//! [`ReadyChannel`]: crate::client::loadbalance::channel_state::ReadyChannel
//! [`ReadyChannel::eject`]: crate::client::loadbalance::channel_state::ReadyChannel::eject
//! [`EjectedChannel`]: crate::client::loadbalance::channel_state::EjectedChannel
//! [`UnejectedChannel`]: crate::client::loadbalance::channel_state::UnejectedChannel

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::watch;

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::channel_state::OutlierChannelState;
use crate::common::async_util::AbortOnDrop;
use crate::xds::resource::outlier_detection::OutlierDetectionConfig;

/// Shared outlier-detection state, owned by `Arc` and accessed by
/// the housekeeping actor ([`Self::run_housekeeping`]) and the load
/// balancer ([`Self::note_uneject`], [`Self::remaining_ejection`]).
pub(crate) struct OutlierStatsRegistry {
    channels: DashMap<EndpointAddress, Arc<OutlierChannelState>>,
    /// Channels currently ejected. Drives the
    /// `max_ejection_percent` cap. Bumped by the sweep on each
    /// ejection; decremented by [`Self::note_uneject`] and
    /// [`Self::remove_channel`].
    ejected_count: AtomicU64,
    /// Monotonic counter bumped every time the ejected-channel set's
    /// membership changes (sweep ejects, LB unejects, removed entry
    /// was ejected). Lets the sweep skip recomputing+broadcasting the
    /// snapshot on quiet ticks via an O(1) compare against
    /// [`Self::last_broadcast_version`].
    ejected_set_version: AtomicU64,
    /// The version that was last broadcast on
    /// [`Self::ejected_snapshot_tx`]. Single-writer (the sweep), so
    /// `Relaxed` is enough.
    last_broadcast_version: AtomicU64,
    /// Shared config, hot-swappable. Readers `.load()` per call;
    /// future xDS integration `.store()`s new configs on cluster
    /// updates. `interval` changes also require an actor restart —
    /// see [`spawn_actor`].
    config: Arc<ArcSwap<OutlierDetectionConfig>>,
    /// Broadcasts the snapshot of currently-ejected addresses at the
    /// end of each sweep that mutated the set. The LB's
    /// [`OutlierDetector`] holds the matching `watch::Receiver` and
    /// diffs against its own `ejected` map. Wrapped in `Arc` so each
    /// receiver clone is cheap regardless of cluster size.
    ejected_snapshot_tx: watch::Sender<Arc<HashSet<EndpointAddress>>>,
}

impl OutlierStatsRegistry {
    /// Construct the registry and the paired snapshot receiver.
    /// The LB owns the receiver; the registry owns the sender.
    pub(crate) fn new(
        config: Arc<ArcSwap<OutlierDetectionConfig>>,
    ) -> (Arc<Self>, watch::Receiver<Arc<HashSet<EndpointAddress>>>) {
        let (tx, rx) = watch::channel(Arc::new(HashSet::new()));
        let registry = Arc::new(Self {
            channels: DashMap::new(),
            ejected_count: AtomicU64::new(0),
            ejected_set_version: AtomicU64::new(0),
            last_broadcast_version: AtomicU64::new(0),
            config,
            ejected_snapshot_tx: tx,
        });
        (registry, rx)
    }

    /// Get or create the state for `addr`. Idempotent — existing
    /// state is preserved across reconnect.
    pub(crate) fn add_channel(&self, addr: EndpointAddress) -> Arc<OutlierChannelState> {
        self.channels
            .entry(addr.clone())
            .or_insert_with(|| Arc::new(OutlierChannelState::new(addr)))
            .clone()
    }

    /// Drop the state for `addr`, decrementing `ejected_count` if
    /// the removed channel was contributing to it.
    pub(crate) fn remove_channel(&self, addr: &EndpointAddress) {
        if let Some((_, state)) = self.channels.remove(addr)
            && state.is_ejected()
        {
            self.ejected_count.fetch_sub(1, Ordering::Relaxed);
            self.ejected_set_version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Number of registered channels.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.channels.len()
    }

    /// Clear the ejection: flip the state, decrement
    /// `ejected_count`, and decrement the multiplier (gRFC A50
    /// step 6.b: same sweep that un-ejects also decrements). Returns
    /// `true` on the ejected → not-ejected transition.
    pub(crate) fn note_uneject(&self, state: &OutlierChannelState) -> bool {
        if state.try_uneject() {
            self.ejected_count.fetch_sub(1, Ordering::Relaxed);
            self.ejected_set_version.fetch_add(1, Ordering::Relaxed);
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
        let config = self.config.load();
        let cap = config.base_ejection_time.max(config.max_ejection_time);
        let target = config
            .base_ejection_time
            .checked_mul(multiplier)
            .unwrap_or(cap)
            .min(cap);
        Some(target.checked_sub(elapsed).unwrap_or_default())
    }

    /// One interval-boundary sweep (gRFC A50 §6). Order matters:
    ///
    /// 1. Snapshot every channel's counters for one consistent pass.
    /// 2. Run the failure-percentage algorithm against the snapshot:
    ///    apply `minimum_hosts` to the qualifying population, then
    ///    `max_ejection_percent`, then per-channel threshold and the
    ///    enforcement roll.
    /// 3. Reset counters and decrement multipliers for non-ejected
    ///    channels.
    /// 4. If the ejected-set version changed (sweep ejected at least
    ///    one channel, or the LB unejected between ticks), rebuild
    ///    the snapshot of ejected addresses and broadcast it on the
    ///    `watch` channel. Quiet ticks skip the rebuild via an O(1)
    ///    version compare.
    ///
    /// Un-ejection is *not* driven from here — each `EjectedChannel`
    /// owns its own `Sleep` timer.
    pub(crate) fn run_housekeeping(&self) {
        let config = self.config.load();
        tracing::debug!(
            channels = self.channels.len(),
            ejected = self.ejected_count.load(Ordering::Relaxed),
            "outlier detection: sweep tick with config {:?}",
            **config,
        );
        let snapshots: Vec<(Arc<OutlierChannelState>, u64, u64)> = self
            .channels
            .iter()
            .map(|e| {
                let state = e.value().clone();
                let (s, f) = state.counters();
                (state, s, f)
            })
            .collect();

        if let Some(fp) = config.failure_percentage.as_ref() {
            let request_volume = u64::from(fp.request_volume);
            let qualifying = snapshots
                .iter()
                .filter(|(_, s, f)| s + f >= request_volume)
                .count() as u64;
            if qualifying >= u64::from(fp.minimum_hosts) {
                let max_ejections = self.max_ejections(&config);
                let now = Instant::now();
                let threshold = u64::from(fp.threshold.get());
                let enforcing = fp.enforcing_failure_percentage.get();
                for (state, s, f) in &snapshots {
                    let total = s + f;
                    if total < request_volume || state.is_ejected() {
                        continue;
                    }
                    if self.ejected_count.load(Ordering::Relaxed) >= max_ejections {
                        break;
                    }
                    // failure_pct = 100 * failure / total. A50 uses strict ">".
                    let failure_pct = 100 * f / total;
                    if failure_pct <= threshold {
                        continue;
                    }
                    if !roll(enforcing) {
                        continue;
                    }
                    if state.try_eject(now) {
                        self.ejected_count.fetch_add(1, Ordering::Relaxed);
                        self.ejected_set_version.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        for (state, _, _) in &snapshots {
            state.snapshot_and_reset();
            if !state.is_ejected() {
                state.decrement_multiplier();
            }
        }

        // Broadcast the ejected-set snapshot, but only if something
        // changed since the last broadcast. Single writer (this task),
        // so `Relaxed` on `last_broadcast_version` is sound.
        let current = self.ejected_set_version.load(Ordering::Relaxed);
        if current != self.last_broadcast_version.load(Ordering::Relaxed) {
            let snapshot: HashSet<EndpointAddress> = self
                .channels
                .iter()
                .filter(|e| e.value().is_ejected())
                .map(|e| e.key().clone())
                .collect();
            tracing::debug!(
                version = current,
                ejected = snapshot.len(),
                "outlier detection: broadcasting ejected-set snapshot {snapshot:?}",
            );
            // Send failure (no receivers) is fine — the LB is being
            // torn down.
            let _ = self.ejected_snapshot_tx.send(Arc::new(snapshot));
            self.last_broadcast_version
                .store(current, Ordering::Relaxed);
        }
    }

    /// Resolve `max_ejection_percent` against the current channel
    /// count. A50 mandates "at least one address regardless of the
    /// value" — without this floor the default 10% × small clusters
    /// (e.g. 5 endpoints) rounds to zero and silently disables
    /// ejection. An empty pool genuinely has nothing to eject.
    fn max_ejections(&self, config: &OutlierDetectionConfig) -> u64 {
        let len = self.channels.len() as u64;
        let cap = len * u64::from(config.max_ejection_percent.get()) / 100;
        if len > 0 { cap.max(1) } else { 0 }
    }
}

/// Spawn the housekeeping actor. Ticks every `config.interval` and
/// calls [`OutlierStatsRegistry::run_housekeeping`]. Dropping the
/// returned [`AbortOnDrop`] stops the task.
///
/// The `interval` is captured at spawn time; live updates require an
/// actor restart, which the xDS-integration layer will own. Other
/// config fields are re-read from the ArcSwap on each tick.
pub(crate) fn spawn_actor(registry: Arc<OutlierStatsRegistry>) -> AbortOnDrop {
    let interval = registry.config.load().interval;
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            registry.run_housekeeping();
        }
    });
    AbortOnDrop(task)
}

/// Per-LB outlier-detection plumbing: shared registry, snapshot
/// receiver, and (when enabled) the housekeeping actor handle
/// (aborted on drop). The LB always owns one of these; the actor is
/// conditional on the config being enabled at construction.
pub(crate) struct OutlierDetector {
    registry: Arc<OutlierStatsRegistry>,
    /// Stream of ejected-address snapshots, broadcast by the sweep
    /// whenever its set changes. `WatchStream::poll_next` yields the
    /// current value on first poll, then yields the new value on each
    /// subsequent change.
    ejected_snapshot_stream: tokio_stream::wrappers::WatchStream<Arc<HashSet<EndpointAddress>>>,
    /// `None` while config is disabled — `record_outcome` short-
    /// circuits and the sweep doesn't run, so nothing ever writes
    /// to the snapshot channel.
    _actor: Option<AbortOnDrop>,
}

impl OutlierDetector {
    /// Pair the registry with the snapshot receiver and (if the
    /// config currently has an algorithm enabled) spawn the
    /// housekeeping actor.
    pub(crate) fn new(
        registry: Arc<OutlierStatsRegistry>,
        ejected_snapshot_rx: watch::Receiver<Arc<HashSet<EndpointAddress>>>,
    ) -> Self {
        let _actor = registry
            .config
            .load()
            .is_enabled()
            .then(|| spawn_actor(registry.clone()));
        Self {
            registry,
            ejected_snapshot_stream: tokio_stream::wrappers::WatchStream::new(ejected_snapshot_rx),
            _actor,
        }
    }

    /// Shared registry handle.
    pub(crate) fn registry(&self) -> &Arc<OutlierStatsRegistry> {
        &self.registry
    }

    /// Poll for the next ejected-set snapshot. `Poll::Ready(Some(_))`
    /// when the sweep broadcasts a new set (or on the first poll, with
    /// the initial empty set). `Poll::Ready(None)` when the sender has
    /// been dropped — i.e. the registry is being torn down.
    pub(crate) fn poll_ejected_snapshot(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Arc<HashSet<EndpointAddress>>>> {
        use futures_util::Stream;
        Pin::new(&mut self.ejected_snapshot_stream).poll_next(cx)
    }
}

/// Return true with probability `pct / 100` (clamped at 100 ⇒ always).
fn roll(pct: u8) -> bool {
    if pct >= 100 {
        return true;
    }
    if pct == 0 {
        return false;
    }
    fastrand::u32(0..100) < u32::from(pct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::outlier_detection::{
        FailurePercentageConfig, OutlierDetectionConfig, Percentage,
    };
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("10.0.0.1", port)
    }

    fn pct(v: u32) -> Percentage {
        Percentage::new(v).unwrap()
    }

    /// Build a registry whose config will never be swapped — these
    /// tests exercise algorithm correctness, not config live-update.
    fn make_registry(
        config: OutlierDetectionConfig,
    ) -> (
        Arc<OutlierStatsRegistry>,
        watch::Receiver<Arc<HashSet<EndpointAddress>>>,
    ) {
        OutlierStatsRegistry::new(Arc::new(ArcSwap::from_pointee(config)))
    }

    /// Convenience wrapper for tests that don't observe ejections.
    fn make_registry_only(config: OutlierDetectionConfig) -> Arc<OutlierStatsRegistry> {
        make_registry(config).0
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

    /// Drive `n` outcomes through `record_outcome` for one channel.
    fn drive(state: &OutlierChannelState, successes: u64, failures: u64) {
        for _ in 0..successes {
            state.record_outcome(true);
        }
        for _ in 0..failures {
            state.record_outcome(false);
        }
    }

    // ----- run_housekeeping: failure-percentage detection -----

    #[test]
    fn ejects_above_threshold_at_sweep() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 10, 90);
        // Per A50 the algorithm runs at the interval sweep, not per RPC.
        assert!(!bad.is_ejected());
        registry.run_housekeeping();
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn skips_below_threshold() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            // 30% failure → below 50% threshold.
            drive(&s, 70, 30);
            all.push(s);
        }
        registry.run_housekeeping();
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn at_threshold_does_not_eject() {
        // A50 specifies a strict "greater than" comparison.
        let registry = make_registry_only(fp_config(50, 10, 3));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&s, 50, 50);
            all.push(s);
        }
        registry.run_housekeeping();
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn minimum_hosts_gates_ejection() {
        let registry = make_registry_only(fp_config(50, 10, 5));
        // Only 2 hosts have request_volume ≥ 10; minimum_hosts is 5 ⇒ skip.
        let mut all = vec![];
        for port in 8080..=8081 {
            let s = registry.add_channel(addr(port));
            drive(&s, 0, 100);
            all.push(s);
        }
        registry.run_housekeeping();
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn request_volume_filters_low_traffic() {
        let registry = make_registry_only(fp_config(50, 100, 3));
        let bad = registry.add_channel(addr(8080));
        drive(&bad, 0, 5);
        for port in 8081..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&s, 200, 0);
        }
        registry.run_housekeeping();
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
        let registry = make_registry_only(config);
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&s, 0, 100);
            all.push(s);
        }
        registry.run_housekeeping();
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    #[test]
    fn max_ejection_percent_caps_concurrent_ejections() {
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(20);
        let registry = make_registry_only(config);

        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            all.push(s);
        }
        // Drive all hosts to bad state.
        for s in &all {
            drive(s, 0, 100);
        }
        registry.run_housekeeping();

        let ejected = all.iter().filter(|s| s.is_ejected()).count();
        // 5 hosts × 20% = 1 max ejection.
        assert_eq!(ejected, 1);
    }

    /// A50 §"max_ejection_percent": at least one address may be
    /// ejected regardless of the percentage. 5 hosts × 10% = 0
    /// arithmetically; the floor still allows 1.
    #[test]
    fn max_ejection_percent_permits_at_least_one_ejection() {
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(10);
        let registry = make_registry_only(config);

        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            all.push(s);
        }
        for s in &all {
            drive(s, 0, 100);
        }
        registry.run_housekeeping();

        let ejected = all.iter().filter(|s| s.is_ejected()).count();
        assert_eq!(ejected, 1);
    }

    #[test]
    fn remove_channel_decrements_ejected_count() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        let mut all = vec![];
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
            all.push(s);
        }
        let bad = registry.add_channel(addr(8084));
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);

        registry.remove_channel(&addr(8084));
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ejection_broadcasts_via_snapshot_watch() {
        let (registry, mut rx) = make_registry(fp_config(50, 10, 3));
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 10, 90);
        registry.run_housekeeping();

        // The snapshot contains exactly the ejected address.
        rx.mark_changed();
        let snapshot = rx.borrow_and_update().clone();
        assert!(snapshot.contains(&addr(8084)));
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn quiet_sweep_does_not_rebroadcast_snapshot() {
        let (registry, rx) = make_registry(fp_config(50, 10, 3));
        for port in 8080..=8084 {
            registry.add_channel(addr(port));
        }
        // First sweep with no qualifying traffic ⇒ no eject ⇒ no broadcast.
        registry.run_housekeeping();
        assert!(
            !rx.has_changed().unwrap(),
            "expected no broadcast on a sweep with no ejection-set changes"
        );
    }

    // ----- Housekeeping -----

    #[test]
    fn housekeeping_resets_counters() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }

        registry.run_housekeeping();
        for port in 8080..=8083 {
            let s = registry.channels.get(&addr(port)).unwrap();
            assert_eq!(s.counters(), (0, 0));
        }
    }

    #[test]
    fn housekeeping_decrements_multiplier_on_healthy_interval() {
        let registry = make_registry_only(base_config());
        let s = registry.add_channel(addr(8080));
        // Force multiplier to 3 directly (no traffic, no eject).
        s.set_ejection_multiplier(3);

        registry.run_housekeeping();
        assert_eq!(s.ejection_multiplier(), 2);
    }

    #[test]
    fn housekeeping_leaves_ejected_multipliers_alone() {
        let registry = make_registry_only(base_config());
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
        let registry = make_registry_only(config);
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
        let registry = make_registry_only(config);
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
        let registry = make_registry_only(config);
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
        let registry = make_registry_only(config);
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
        let registry = make_registry_only(base_config());
        let s = registry.add_channel(addr(8080));
        assert!(registry.remaining_ejection(&s, Instant::now()).is_none());
    }

    #[test]
    fn note_uneject_clears_state_and_decrements_counter() {
        let registry = make_registry_only(base_config());
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
        let registry = make_registry_only(config);
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
        let registry = make_registry_only(config);
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
