//! Power-of-two-choices (P2C) channel picker.

use indexmap::IndexMap;
use tower::load::Load;

use crate::client::endpoint::EndpointAddress;
use crate::client::loadbalance::pickers::ChannelPicker;

/// Pick two distinct random indices from `[0, length)` using Floyd's algorithm.
fn sample_floyd2(length: usize) -> [usize; 2] {
    debug_assert!(length >= 2);
    let a = fastrand::usize(..length - 1);
    let b = fastrand::usize(..length);
    let a = if a == b { length - 1 } else { a };
    [a, b]
}

/// Picks the least-loaded of two randomly chosen endpoints.
pub(crate) struct P2cPicker;

impl<S, Req> ChannelPicker<S, Req> for P2cPicker
where
    S: Load,
    S::Metric: PartialOrd,
{
    fn pick<'a>(&self, _req: &Req, ready: &'a IndexMap<EndpointAddress, S>) -> Option<&'a S> {
        let len = ready.len();
        match len {
            0 => None,
            1 => ready.get_index(0).map(|(_, s)| s),
            _ => {
                let [a, b] = sample_floyd2(len);
                let (_, ch_a) = ready.get_index(a)?;
                let (_, ch_b) = ready.get_index(b)?;
                if ch_a.load() <= ch_b.load() {
                    Some(ch_a)
                } else {
                    Some(ch_b)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A minimal Load impl for testing. Stores a fixed load value.
    struct MockChannel {
        load: AtomicU64,
    }

    impl MockChannel {
        fn new(load: u64) -> Self {
            Self {
                load: AtomicU64::new(load),
            }
        }
    }

    impl Load for MockChannel {
        type Metric = u64;
        fn load(&self) -> Self::Metric {
            self.load.load(Ordering::Relaxed)
        }
    }

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", port)
    }

    #[test]
    fn test_pick_empty_returns_none() {
        let ready: IndexMap<EndpointAddress, MockChannel> = IndexMap::new();
        assert!(P2cPicker.pick(&(), &ready).is_none());
    }

    #[test]
    fn test_pick_single_returns_only_endpoint() {
        let mut ready = IndexMap::new();
        ready.insert(addr(8080), MockChannel::new(42));

        let picked = P2cPicker.pick(&(), &ready).unwrap();
        assert_eq!(picked.load(), 42);
    }

    #[test]
    fn test_pick_two_prefers_lower_load() {
        let mut ready = IndexMap::new();
        ready.insert(addr(8080), MockChannel::new(100));
        ready.insert(addr(8081), MockChannel::new(0));

        // With only 2 endpoints, P2C always compares both.
        for _ in 0..100 {
            let picked = P2cPicker.pick(&(), &ready).unwrap();
            assert_eq!(
                picked.load(),
                0,
                "should always pick the lower-loaded endpoint"
            );
        }
    }

    #[test]
    fn test_pick_equal_load_returns_some() {
        let mut ready = IndexMap::new();
        ready.insert(addr(8080), MockChannel::new(5));
        ready.insert(addr(8081), MockChannel::new(5));
        ready.insert(addr(8082), MockChannel::new(5));

        for _ in 0..100 {
            let picked = P2cPicker.pick(&(), &ready).unwrap();
            assert_eq!(picked.load(), 5);
        }
    }

    #[test]
    fn test_pick_many_endpoints_distributes() {
        let mut ready = IndexMap::new();
        for port in 8080..8090 {
            // Encode the port in the load so we can identify which one was picked.
            ready.insert(addr(port), MockChannel::new(port as u64));
        }

        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let picked = P2cPicker.pick(&(), &ready).unwrap();
            seen.insert(picked.load());
        }
        // With 10 endpoints and 1000 picks, we should hit most of them.
        assert!(
            seen.len() >= 8,
            "expected to hit most endpoints, only hit {}",
            seen.len()
        );
    }
}
