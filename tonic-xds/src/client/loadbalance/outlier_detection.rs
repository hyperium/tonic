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
//!   `config.interval` tick, snapshots and resets every channel's
//!   counters, runs the success-rate and failure-percentage algorithms
//!   over that snapshot, ejects qualifying channels, and decrements
//!   multipliers for non-ejected channels. When the ejected-set membership changes,
//!   broadcasts a fresh snapshot on the `watch` channel; quiet ticks
//!   skip the broadcast via an O(1) version compare.
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
    /// 1. Snapshot and reset every channel's counters for one
    ///    consistent pass (A50 §2 swaps the counter buckets before the
    ///    algorithms run).
    /// 2. Run the success-rate algorithm against the snapshot: compute
    ///    mean and stdev of success rates across qualifying hosts (per
    ///    `request_volume`), gated by `minimum_hosts`; eject any host
    ///    whose success rate is below `mean - stdev * stdev_factor /
    ///    1000`, subject to `max_ejection_percent` and the enforcement
    ///    roll.
    /// 3. Run the failure-percentage algorithm against the same
    ///    snapshot: apply `minimum_hosts` to the qualifying population,
    ///    then `max_ejection_percent`, then per-channel threshold and
    ///    the enforcement roll. Hosts already ejected by step 2 are
    ///    skipped, and the `max_ejection_percent` cap accounts for them.
    /// 4. Decrement multipliers for non-ejected channels (counters were
    ///    already reset in step 1).
    /// 5. If the ejected-set version changed (sweep ejected at least
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
                // A50 step 2 swaps (resets) the counter buckets *before* the
                // algorithms run, so an outcome that lands mid-sweep accrues to
                // the fresh bucket instead of being dropped by a later reset.
                let (s, f) = state.snapshot_and_reset();
                (state, s, f)
            })
            .collect();

        if let Some(sr) = config.success_rate.as_ref() {
            let request_volume = u64::from(sr.request_volume);
            // Success rate in 0.0..=100.0 for each qualifying host with
            // traffic; a zero-total host has no defined rate and is
            // excluded so mean/stdev stay finite. The threshold is
            // `mean - stdev * stdev_factor / 1000` (A50 §"success_rate
            // ejection").
            let rates: Vec<f64> = snapshots
                .iter()
                .filter_map(|(_, s, f)| {
                    let total = s + f;
                    (total >= request_volume && total > 0)
                        .then(|| 100.0 * (*s as f64) / (total as f64))
                })
                .collect();
            if rates.len() >= sr.minimum_hosts as usize && !rates.is_empty() {
                let n = rates.len() as f64;
                let mean = rates.iter().sum::<f64>() / n;
                let variance = rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
                let stdev = variance.sqrt();
                let threshold = mean - stdev * f64::from(sr.stdev_factor) / 1000.0;
                self.eject_outliers(
                    &snapshots,
                    request_volume,
                    config.max_ejection_percent.get(),
                    sr.enforcing_success_rate.get(),
                    |s, _f, total| 100.0 * (s as f64) / (total as f64) < threshold,
                );
            }
        }

        if let Some(fp) = config.failure_percentage.as_ref() {
            let request_volume = u64::from(fp.request_volume);
            let qualifying = snapshots
                .iter()
                .filter(|(_, s, f)| s + f >= request_volume)
                .count() as u64;
            if qualifying >= u64::from(fp.minimum_hosts) {
                let threshold = u64::from(fp.threshold.get());
                self.eject_outliers(
                    &snapshots,
                    request_volume,
                    config.max_ejection_percent.get(),
                    fp.enforcing_failure_percentage.get(),
                    // failure_pct = 100 * failure / total. A50 uses strict ">".
                    |_s, f, total| 100 * f / total > threshold,
                );
            }
        }

        // Counters were already reset when the snapshot was taken (A50 step 2),
        // so this pass only decrements the ejection-time multiplier for hosts
        // that are still healthy (A50 step 5).
        for (state, _, _) in &snapshots {
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

    /// Shared ejection pass for one detection algorithm: walks
    /// `snapshots`, skips idle or already-ejected hosts, respects the
    /// concurrent-ejection cap, and ejects a host when `is_outlier`
    /// flags it and the enforcement roll passes. Centralizing the loop
    /// keeps the success-rate and failure-percentage paths from
    /// drifting apart.
    ///
    /// `is_outlier` sees `(success, failure, total)` for a host with
    /// `total > 0`, so the per-algorithm ratio never divides by zero.
    fn eject_outliers(
        &self,
        snapshots: &[(Arc<OutlierChannelState>, u64, u64)],
        request_volume: u64,
        max_ejection_percent: u8,
        enforcing: u8,
        is_outlier: impl Fn(u64, u64, u64) -> bool,
    ) {
        let endpoint_count = snapshots.len() as u64;
        let now = Instant::now();
        for (state, s, f) in snapshots {
            let (s, f) = (*s, *f);
            let total = s + f;
            if total == 0 || total < request_volume || state.is_ejected() {
                continue;
            }
            if !self.can_eject_more(endpoint_count, max_ejection_percent) {
                break;
            }
            if !is_outlier(s, f, total) {
                continue;
            }
            if !roll(enforcing) {
                continue;
            }
            self.try_eject_with_guard(state, now);
        }
    }

    /// Eject `state` only while it is still the registered channel for its
    /// address. Holding the map entry across `try_eject` closes a race: the
    /// sweep snapshots a not-yet-ejected host, a concurrent EDS update calls
    /// `remove_channel` (which decrements nothing, since the host wasn't
    /// ejected), and the sweep then ejects the stale snapshot. Without the
    /// guard that bumps `ejected_count` for a host that is already gone, and
    /// nothing ever balances it — the count stays inflated and throttles future
    /// ejections. A `ptr_eq` check also rejects an address that was removed and
    /// re-added as a fresh state.
    fn try_eject_with_guard(&self, state: &Arc<OutlierChannelState>, now: Instant) {
        let Some(entry) = self.channels.get(state.addr()) else {
            return;
        };
        if !Arc::ptr_eq(entry.value(), state) {
            return;
        }
        if state.try_eject(now) {
            self.ejected_count.fetch_add(1, Ordering::Relaxed);
            self.ejected_set_version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// gRFC A50 checks the ejection cap *before* each ejection: "If the
    /// percentage of ejected addresses is greater than or equal to
    /// `max_ejection_percent`, stop." Evaluating it per-ejection rather than
    /// precomputing `count * pct / 100` matches the spec — a 4-endpoint cluster
    /// at 30% ejects 2 (0% and 25% are both below 30%), whereas the truncated
    /// precomputed cap `floor(1.2) = 1` under-ejects. The first ejection is
    /// always allowed (A50 "at least one address regardless of the value"); an
    /// empty pool has nothing to eject.
    ///
    /// Integer division is exact: `floor(100 * ejected / count) < pct` iff
    /// `100 * ejected / count < pct`, because `pct` is a whole number.
    fn can_eject_more(&self, endpoint_count: u64, max_ejection_percent: u8) -> bool {
        if endpoint_count == 0 {
            return false;
        }
        let ejected = self.ejected_count.load(Ordering::Relaxed);
        if ejected == 0 {
            return true;
        }
        100 * ejected / endpoint_count < u64::from(max_ejection_percent)
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
        FailurePercentageConfig, OutlierDetectionConfig, Percentage, SuccessRateConfig,
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
    /// arithmetically; the first ejection is always allowed.
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

    /// A50 re-checks the ejected percentage before each ejection, so a
    /// 4-endpoint cluster at 30% ejects 2 (0% and 25% are below 30%; 50%
    /// stops). The old precomputed cap `floor(4 * 30 / 100) = 1` under-ejected.
    #[test]
    fn max_ejection_percent_ejects_up_to_a50_boundary() {
        let mut config = fp_config(50, 10, 3);
        config.max_ejection_percent = pct(30);
        let registry = make_registry_only(config);

        let mut all = vec![];
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 0, 100);
            all.push(s);
        }
        registry.run_housekeeping();

        let ejected = all.iter().filter(|s| s.is_ejected()).count();
        assert_eq!(ejected, 2);
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

    /// If a host is removed after the sweep snapshots it but before it is
    /// ejected, the guard must not eject the stale snapshot and leak
    /// `ejected_count` (nothing would ever balance it).
    #[test]
    fn eject_guard_skips_removed_channel() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        let a = registry.add_channel(addr(8080));
        registry.remove_channel(&addr(8080));

        registry.try_eject_with_guard(&a, Instant::now());

        assert!(!a.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 0);
    }

    /// The guard still ejects a host that is present and unchanged.
    #[test]
    fn eject_guard_ejects_present_channel() {
        let registry = make_registry_only(fp_config(50, 10, 3));
        let a = registry.add_channel(addr(8080));

        registry.try_eject_with_guard(&a, Instant::now());

        assert!(a.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
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

    // ----- run_housekeeping: success-rate detection -----

    /// 4 hosts at 100%, 1 at 0%. mean=80, stdev=40, threshold with
    /// factor 1900 = 80 - 40 * 1.9 = 4 ⇒ the 0% host (rate < 4) is
    /// ejected; the others are clear.
    #[test]
    fn success_rate_ejects_outlier_below_threshold() {
        let registry = make_registry_only(sr_config(1900, 10, 3));
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
    }

    /// Uniform population: stdev = 0, threshold = mean, no host is
    /// strictly below the mean ⇒ nothing ejects.
    #[test]
    fn success_rate_uniform_population_does_not_eject() {
        let registry = make_registry_only(sr_config(1900, 10, 3));
        let mut all = vec![];
        for port in 8080..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&s, 80, 20);
            all.push(s);
        }
        registry.run_housekeeping();
        for s in &all {
            assert!(!s.is_ejected());
        }
    }

    /// minimum_hosts boundary: with exactly `minimum_hosts` qualifying hosts the
    /// gate opens (`>=`) and the lone outlier is ejected. A `>` would skip the
    /// algorithm and leave it un-ejected, so this pins the comparison — a case
    /// far below the minimum can't, since `>=` and `>` behave identically there.
    #[test]
    fn success_rate_minimum_hosts_boundary_ejects_outlier() {
        let registry = make_registry_only(sr_config(1000, 10, 3));
        let bad = registry.add_channel(addr(8082));
        for port in 8080..=8081 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        assert!(bad.is_ejected());
    }

    /// request_volume filter: the low-traffic outlier is excluded from
    /// both the qualifying population and the candidate list, so even
    /// though its rate is 0%, it doesn't get ejected.
    #[test]
    fn success_rate_request_volume_filters_low_traffic() {
        let registry = make_registry_only(sr_config(1900, 100, 3));
        let bad = registry.add_channel(addr(8080));
        drive(&bad, 0, 5);
        for port in 8081..=8084 {
            let s = registry.add_channel(addr(port));
            drive(&s, 200, 0);
        }
        registry.run_housekeeping();
        assert!(!bad.is_ejected());
    }

    /// `enforcing_success_rate = 0` skips actual ejection regardless
    /// of how far below threshold a host falls.
    #[test]
    fn success_rate_enforcement_zero_never_ejects() {
        let mut config = sr_config(1900, 10, 3);
        config.success_rate.as_mut().unwrap().enforcing_success_rate = pct(0);
        let registry = make_registry_only(config);
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        assert!(!bad.is_ejected());
    }

    /// stdev_factor 0 collapses the threshold to the mean. 4 hosts at
    /// 100% + 1 at 0% gives mean=80, so the 0% host (< 80) ejects but
    /// the 100% hosts (not < 80) don't.
    #[test]
    fn success_rate_stdev_factor_zero_ejects_below_mean() {
        let registry = make_registry_only(sr_config(0, 10, 3));
        let bad = registry.add_channel(addr(8084));
        let mut healthy = vec![];
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
            healthy.push(s);
        }
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        assert!(bad.is_ejected());
        for s in &healthy {
            assert!(!s.is_ejected());
        }
    }

    /// The cap bounds concurrent ejections below the number of eligible
    /// outliers: two hosts fall below threshold but `5 × 20% = 1` admits
    /// only one, so exactly one is ejected.
    #[test]
    fn success_rate_max_ejection_percent_caps_concurrent_ejections() {
        let mut config = sr_config(1000, 10, 3);
        config.max_ejection_percent = pct(20);
        let registry = make_registry_only(config);
        // 3 hosts at 100%, 2 at 0%. Both zero-rate hosts fall below the
        // threshold, so without the cap both would eject; the cap holds
        // the second one back.
        let bad1 = registry.add_channel(addr(8083));
        let bad2 = registry.add_channel(addr(8084));
        for port in 8080..=8082 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad1, 0, 100);
        drive(&bad2, 0, 100);
        registry.run_housekeeping();
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
        assert!(bad1.is_ejected() ^ bad2.is_ejected());
    }

    /// Both algorithms configured: success-rate runs first and
    /// catches the cross-host outlier; failure-percentage gets a
    /// second look but skips already-ejected hosts.
    #[test]
    fn success_rate_and_failure_percentage_compose() {
        let mut config = sr_config(1900, 10, 3);
        config.failure_percentage = Some(FailurePercentageConfig {
            threshold: pct(50),
            enforcing_failure_percentage: pct(100),
            minimum_hosts: 3,
            request_volume: 10,
        });
        let registry = make_registry_only(config);
        let bad = registry.add_channel(addr(8084));
        for port in 8080..=8083 {
            let s = registry.add_channel(addr(port));
            drive(&s, 100, 0);
        }
        drive(&bad, 0, 100);
        registry.run_housekeeping();
        // Success-rate ejected it; failure-percentage saw it as
        // already-ejected on its pass and didn't double-count.
        assert!(bad.is_ejected());
        assert_eq!(registry.ejected_count.load(Ordering::Relaxed), 1);
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
