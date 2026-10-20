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

//! Ring-hash channel picker (gRFC A42).
//!
//! Builds a hash ring over the cluster's full healthy-EDS membership and routes
//! each request to the endpoint owning the first ring position at or after the
//! request's hash. Same hash → same endpoint, so requests carrying the same
//! hash key stick to the same backend (affinity).
//!
//! The ring is rebuilt only when membership changes (via
//! [`ChannelPicker::on_members_changed`]); connection flaps and outlier
//! ejections do not touch it. At pick time the chosen ring entry is resolved
//! against the `ready` set — an entry whose host is not ready (connecting,
//! ejected, or never connected) is skipped and the walk continues clockwise to
//! the next ready host, returning `None` (→ `Unavailable`) if none is ready.
//!
//! Currently it supports uniform per-member weighting and an eager-connect
//! pick: the walk selects the first ready host.

use std::fmt::Write;
use std::sync::Arc;

use arc_swap::ArcSwap;
use indexmap::{IndexMap, IndexSet};

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::pickers::ChannelPicker;
use crate::client::route::RouteDecision;

/// Ring-hash LB configuration (gRFC A42 `ring_hash_lb_config`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RingHashConfig {
    /// Minimum number of entries in the ring.
    pub min_ring_size: u64,
    /// Maximum number of entries in the ring.
    pub max_ring_size: u64,
}

impl Default for RingHashConfig {
    /// gRFC A42 defaults: `min_ring_size` 1024, `max_ring_size` 4096.
    fn default() -> Self {
        Self {
            min_ring_size: 1024,
            max_ring_size: 4096,
        }
    }
}

/// One ring entry: a hash position mapped to the endpoint that owns it.
struct RingEntry {
    hash: u64,
    addr: EndpointAddress,
}

/// A built hash ring: entries kept sorted by hash. [`Ring::new`] is the only
/// way to construct one, so a `Ring` is always sorted and the binary search in
/// [`Ring::pick`] is sound.
struct Ring(Vec<RingEntry>);

impl Ring {
    /// Build the ring over `members` with uniform weights. The ring size
    /// is the smallest multiple of `N` that is ≥ `min_ring_size` — the
    /// uniform-weight case of A42's "smallest number giving the smallest-weight
    /// host a whole number of entries" — hard-clamped to `max_ring_size`. Those
    /// entries are spread across members by cumulative rounding, so each gets
    /// floor or ceil of `ring_size / N` and the total is exactly `ring_size`;
    /// when members outnumber the clamped size the trailing ones get zero
    /// entries. Each entry is keyed
    /// `xxh64("{addr}_{i}", 0)`, `i` being the member's previous appearance
    /// count, and entries are then sorted by hash.
    fn new(config: &RingHashConfig, members: &IndexSet<EndpointAddress>) -> Self {
        let n = members.len() as u64;
        if n == 0 {
            return Ring(Vec::new());
        }
        // TODO(madhurishgupta): Full A42 sizes the ring by each endpoint's
        // weight (EDS endpoint weight and locality weight). Currently this uses
        // uniform weights.
        // Smallest multiple of N ≥ min_ring_size (so the smallest-weight host
        // gets a whole number of entries), hard-clamped to max_ring_size.
        let ring_size = config
            .min_ring_size
            .div_ceil(n)
            .saturating_mul(n)
            .min(config.max_ring_size);

        // Spread ring_size entries across members by cumulative rounding:
        // member k owns entries until the running total reaches
        // ceil(ring_size * (k + 1) / N). Each member therefore gets floor or
        // ceil of ring_size / N, the total is exactly ring_size, and when
        // N > ring_size the trailing members get zero entries.
        let mut entries = Vec::with_capacity(ring_size as usize);
        // Reuse one buffer for the per-entry key instead of allocating a fresh
        // String each iteration; `clear` keeps the capacity.
        let mut key = String::new();
        let mut emitted: u64 = 0;
        for (k, addr) in members.iter().enumerate() {
            let target = (ring_size as u128 * (k as u128 + 1)).div_ceil(n as u128) as u64;
            for i in 0..(target - emitted) {
                key.clear();
                write!(key, "{addr}_{i}").expect("writing into a String is infallible");
                entries.push(RingEntry {
                    hash: xxhash_rust::xxh64::xxh64(key.as_bytes(), 0),
                    addr: addr.clone(),
                });
            }
            emitted = target;
        }
        entries.sort_by_key(|e| e.hash);
        Ring(entries)
    }

    /// Pick the channel for `hash`: find the ring position closest to the hash
    /// (first entry with `hash ≥ request`, wrapping around the ring), then walk
    /// clockwise to the first ready host. Returns `None` if the ring is empty
    /// or no ring host is ready.
    fn pick<'a, S>(&self, hash: u64, ready: &'a IndexMap<EndpointAddress, S>) -> Option<&'a S> {
        if self.0.is_empty() {
            return None;
        }
        let start = self.0.partition_point(|e| e.hash < hash);
        let len = self.0.len();
        for off in 0..len {
            let entry = &self.0[(start + off) % len];
            if let Some(channel) = ready.get(&entry.addr) {
                return Some(channel);
            }
        }
        None
    }
}

/// A ring-hash picker. The ring is held behind an [`ArcSwap`] so the hot path
/// reads it lock-free while membership rebuilds publish a new ring atomically.
pub(crate) struct RingHashPicker {
    config: RingHashConfig,
    ring: ArcSwap<Ring>,
}

impl RingHashPicker {
    /// Create a picker with an empty ring. The ring is populated on the first
    /// [`ChannelPicker::on_members_changed`] call.
    pub(crate) fn new(config: RingHashConfig) -> Self {
        Self {
            config,
            ring: ArcSwap::from_pointee(Ring(Vec::new())),
        }
    }

    /// Rebuild the ring over the given members and publish it atomically.
    pub(crate) fn rebuild(&self, members: &IndexSet<EndpointAddress>) {
        self.ring.store(Arc::new(Ring::new(&self.config, members)));
    }
}

impl<S, B> ChannelPicker<S, http::Request<B>> for RingHashPicker {
    fn pick<'a>(
        &self,
        req: &http::Request<B>,
        ready: &'a IndexMap<EndpointAddress, S>,
    ) -> Option<&'a S> {
        if ready.is_empty() {
            return None;
        }
        // The request hash comes from the route's hash policies (gRFC A42). If
        // absent (no policy produced a hash), use a per-request random hash so
        // the request lands somewhere on the ring.
        let hash = req
            .extensions()
            .get::<RouteDecision>()
            .and_then(|d| d.request_hash)
            .unwrap_or_else(|| fastrand::u64(..));
        self.ring.load().pick(hash, ready)
    }

    fn on_members_changed(&self, members: &IndexSet<EndpointAddress>) {
        self.rebuild(members);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::route::RouteDecision;

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", port)
    }

    fn members(ports: &[u16]) -> IndexSet<EndpointAddress> {
        ports.iter().map(|p| addr(*p)).collect()
    }

    /// Build a `ready` map keying each address to a marker value (its port).
    fn ready(ports: &[u16]) -> IndexMap<EndpointAddress, u16> {
        ports.iter().map(|p| (addr(*p), *p)).collect()
    }

    fn req(hash: Option<u64>) -> http::Request<()> {
        let mut r = http::Request::new(());
        r.extensions_mut().insert(RouteDecision {
            cluster: "c".to_string(),
            request_hash: hash,
            retry_config: None,
            max_stream_duration: None,
        });
        r
    }

    /// Full-range hash for sweep tests. Real request hashes are `xxh64` of a
    /// header value and span the whole `u64` space; small integers would all
    /// cluster below the ring and wrap to the same first entry.
    fn spread_hash(i: u64) -> u64 {
        xxhash_rust::xxh64::xxh64(format!("k{i}").as_bytes(), 0)
    }

    // ----- build_ring (gRFC A42 uniform sizing) -----

    #[test]
    fn empty_members_produce_empty_ring() {
        let ring = Ring::new(&RingHashConfig::default(), &IndexSet::new());
        assert!(ring.0.is_empty());
    }

    #[test]
    fn ring_size_is_smallest_multiple_of_n_ge_min() {
        let cfg = RingHashConfig {
            min_ring_size: 1024,
            max_ring_size: 8_000_000,
        };
        // N=3: smallest multiple of 3 ≥ 1024 is 1026 (per_host 342).
        let ring = Ring::new(&cfg, &members(&[8080, 8081, 8082])).0;
        assert_eq!(ring.len(), 1026);
    }

    #[test]
    fn ring_per_host_counts_are_equal() {
        let cfg = RingHashConfig {
            min_ring_size: 1024,
            max_ring_size: 8_000_000,
        };
        // N=4: 1024 is already a multiple of 4 → per_host 256, total 1024.
        let ring = Ring::new(&cfg, &members(&[8080, 8081, 8082, 8083])).0;
        assert_eq!(ring.len(), 1024);
        let mut counts: std::collections::HashMap<EndpointAddress, usize> =
            std::collections::HashMap::new();
        for e in &ring {
            *counts.entry(e.addr.clone()).or_default() += 1;
        }
        assert_eq!(counts.len(), 4);
        assert!(counts.values().all(|&c| c == 256));
    }

    #[test]
    fn ring_size_clamped_to_max_exactly() {
        let cfg = RingHashConfig {
            min_ring_size: 1000,
            max_ring_size: 10,
        };
        // ceil(1000/3)*3 = 1002 > max(10) → ring size hard-capped to exactly 10
        // (per A42), spread 4/3/3 across the members by cumulative rounding.
        let ring = Ring::new(&cfg, &members(&[8080, 8081, 8082])).0;
        assert_eq!(ring.len(), 10);
        let mut counts: std::collections::HashMap<EndpointAddress, usize> =
            std::collections::HashMap::new();
        for e in &ring {
            *counts.entry(e.addr.clone()).or_default() += 1;
        }
        let mut sizes: Vec<usize> = counts.values().copied().collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![3, 3, 4]);
    }

    #[test]
    fn members_outnumbering_max_ring_size_are_capped() {
        // 4 members, max ring size 2: the total is hard-capped at 2, so two
        // members own an entry and two get none — A42's clamp to max_ring_size
        // rather than forcing every member onto the ring.
        let cfg = RingHashConfig {
            min_ring_size: 8,
            max_ring_size: 2,
        };
        let ring = Ring::new(&cfg, &members(&[8080, 8081, 8082, 8083])).0;
        assert_eq!(ring.len(), 2);
        let distinct: std::collections::HashSet<EndpointAddress> =
            ring.iter().map(|e| e.addr.clone()).collect();
        assert_eq!(
            distinct.len(),
            2,
            "exactly two members should own ring entries"
        );
    }

    #[test]
    fn ring_is_deterministic() {
        let cfg = RingHashConfig::default();
        let m = members(&[8080, 8081, 8082]);
        let a = Ring::new(&cfg, &m).0;
        let b = Ring::new(&cfg, &m).0;
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter()
                .zip(&b)
                .all(|(x, y)| x.hash == y.hash && x.addr == y.addr)
        );
    }

    #[test]
    fn ring_is_sorted_by_hash() {
        let ring = Ring::new(&RingHashConfig::default(), &members(&[8080, 8081, 8082])).0;
        assert!(ring.windows(2).all(|w| w[0].hash <= w[1].hash));
    }

    // ----- pick -----

    fn picker(ports: &[u16]) -> RingHashPicker {
        let p = RingHashPicker::new(RingHashConfig::default());
        p.rebuild(&members(ports));
        p
    }

    #[test]
    fn same_hash_picks_same_endpoint() {
        let p = picker(&[8080, 8081, 8082]);
        let ready = ready(&[8080, 8081, 8082]);
        let r = req(Some(0x1234_5678));
        let a = p.pick(&r, &ready).copied();
        let b = p.pick(&r, &ready).copied();
        assert!(a.is_some());
        assert_eq!(a, b);
    }

    #[test]
    fn pick_selects_clockwise_successor_of_hash() {
        // With every host ready, the pick must resolve to the owner of the
        // first ring entry whose hash is ≥ the request hash, wrapping to entry
        // 0 when the hash exceeds them all. Computed independently against the
        // actual ring, so it pins the walk's start.
        let p = picker(&[8080, 8081, 8082]);
        let ready = ready(&[8080, 8081, 8082]);
        let guard = p.ring.load();
        let ring = &guard.0;
        let mid = ring[ring.len() / 2].hash;
        let probes = [
            0,                   // below most entries
            mid.wrapping_sub(1), // just before a known entry
            mid,                 // exactly on a known entry
            u64::MAX,            // above every entry → wraps to entry 0
        ];
        for h in probes {
            // First entry with hash ≥ h, else wrap to entry 0.
            let expected = ring.iter().find(|e| e.hash >= h).unwrap_or(&ring[0]);
            let expected_port = *ready.get(&expected.addr).unwrap();
            assert_eq!(
                p.pick(&req(Some(h)), &ready).copied(),
                Some(expected_port),
                "hash {h:#x} should map to the clockwise-successor entry",
            );
        }
    }

    #[test]
    fn different_hashes_can_pick_different_endpoints() {
        let p = picker(&[8080, 8081, 8082]);
        let ready = ready(&[8080, 8081, 8082]);
        // Sweep many hashes; with 3 endpoints we should hit more than one.
        let mut seen = std::collections::HashSet::new();
        for h in 0..256u64 {
            if let Some(v) = p.pick(&req(Some(spread_hash(h))), &ready).copied() {
                seen.insert(v);
            }
        }
        assert!(
            seen.len() > 1,
            "expected requests to spread across endpoints"
        );
    }

    #[test]
    fn falls_through_to_next_ready_host() {
        // Ring is built over 3 members but only one is ready: every request
        // must fall through the ring to that single ready host.
        let p = picker(&[8080, 8081, 8082]);
        let ready = ready(&[8081]); // only 8081 ready
        for h in 0..64u64 {
            assert_eq!(p.pick(&req(Some(h)), &ready).copied(), Some(8081));
        }
    }

    #[test]
    fn not_ready_host_is_skipped_and_keys_fall_through() {
        // A host that is connecting / ejected / unhealthy is simply absent from
        // `ready`. Requests whose hash lands on its ring arcs must skip it and
        // fall through to the next ready host; all other keys are unaffected.
        let p = picker(&[8080, 8081, 8082]);
        let all_ready = ready(&[8080, 8081, 8082]);
        let without_8081 = ready(&[8080, 8082]); // 8081 not ready

        let mut fell_through = 0;
        for h in 0..512u64 {
            let r = req(Some(spread_hash(h)));
            let with = p.pick(&r, &all_ready).copied().unwrap();
            let without = p.pick(&r, &without_8081).copied().unwrap();

            // The not-ready host is never selected.
            assert_ne!(without, 8081, "not-ready host must not be picked");

            if with == 8081 {
                // Keys owned by the not-ready host fall through to a ready host.
                assert!(without == 8080 || without == 8082);
                fell_through += 1;
            } else {
                // Keys not owned by it keep their endpoint unchanged.
                assert_eq!(with, without, "a ready host's keys must not move");
            }
        }
        assert!(
            fell_through > 0,
            "expected some keys to be owned by the not-ready host (and fall through)"
        );
    }

    #[test]
    fn no_ready_hosts_returns_none() {
        let p = picker(&[8080, 8081, 8082]);
        let ready: IndexMap<EndpointAddress, u16> = IndexMap::new();
        assert!(p.pick(&req(Some(42)), &ready).is_none());
    }

    #[test]
    fn empty_ring_returns_none() {
        let p = RingHashPicker::new(RingHashConfig::default()); // never got members
        let ready = ready(&[8080]);
        assert!(p.pick(&req(Some(42)), &ready).is_none());
    }

    #[test]
    fn missing_request_hash_uses_random_fallback() {
        let p = picker(&[8080, 8081, 8082]);
        let ready = ready(&[8080, 8081, 8082]);
        // No request hash → random hash → still resolves to some ready host.
        assert!(p.pick(&req(None), &ready).is_some());
    }

    #[test]
    fn rebuild_on_members_changed_reflects_new_membership() {
        let p = RingHashPicker::new(RingHashConfig::default());
        p.rebuild(&members(&[8080]));
        let ready_one = ready(&[8080]);
        assert_eq!(p.pick(&req(Some(7)), &ready_one).copied(), Some(8080));

        // Grow membership; the new hosts must be reachable for some hashes.
        p.rebuild(&members(&[8080, 8081, 8082]));
        let ready_all = ready(&[8080, 8081, 8082]);
        let mut seen = std::collections::HashSet::new();
        for h in 0..256u64 {
            if let Some(v) = p.pick(&req(Some(spread_hash(h))), &ready_all).copied() {
                seen.insert(v);
            }
        }
        assert!(seen.contains(&8081) || seen.contains(&8082));
    }
}
