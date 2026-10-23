/*
 *
 * Copyright 2026 gRPC authors.
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

//! Weighted round-robin (WRR) item selection.

use std::fmt::Debug;
use std::sync::Arc;

/// Weighted round-robin selector over a fixed set of items.
pub(crate) trait Wrr<T> {
    /// Selects an item from the set, or returns `None` if no items have a
    /// positive weight.
    fn next(&self) -> Option<&T>;
}

/// Builds [`Wrr`] instances from `(item, weight)` pairs.
pub(crate) trait WrrBuilder<T> {
    type Wrr: Wrr<T>;

    fn build(&self, items: impl IntoIterator<Item = (T, u32)>) -> Self::Wrr;
}

/// Produces the random keys [`RandomWrr`] uses to select an item. Tests supply
/// fixed keys to make selection deterministic.
trait RngSampler: Send + Sync + Debug {
    /// Generates a uniform random value in the half-open range `[0, total_weight)`.
    fn sample(&self, total_weight: u64) -> u64;
}

#[derive(Debug)]
struct DefaultRngSampler;

impl RngSampler for DefaultRngSampler {
    fn sample(&self, total_weight: u64) -> u64 {
        rand::random_range(0..total_weight)
    }
}

/// [`WrrBuilder`] that constructs [`RandomWrr`] instances.
#[derive(Clone, Debug)]
pub(crate) struct RandomWrrBuilder {
    sampler: Arc<dyn RngSampler>,
}

impl Default for RandomWrrBuilder {
    fn default() -> Self {
        Self {
            sampler: Arc::new(DefaultRngSampler),
        }
    }
}

impl RandomWrrBuilder {
    #[cfg(test)]
    fn with_sampler(sampler: Arc<dyn RngSampler>) -> Self {
        Self { sampler }
    }
}

impl<T> WrrBuilder<T> for RandomWrrBuilder {
    type Wrr = RandomWrr<T>;

    fn build(&self, items: impl IntoIterator<Item = (T, u32)>) -> Self::Wrr {
        let mut cumulative_weight = 0u64;
        let items = items
            .into_iter()
            .filter(|(_, weight)| *weight > 0)
            .map(|(item, weight)| {
                cumulative_weight += u64::from(weight);
                (cumulative_weight, item)
            })
            .collect();
        RandomWrr {
            items,
            sampler: self.sampler.clone(),
        }
    }
}

/// Weighted random item selector.
///
/// Stores `(cumulative_weight, item)` pairs in ascending order of cumulative
/// weight and selects items in proportion to their weight by binary-searching
/// for a uniform sample in `[0, total_weight)`.
#[derive(Debug)]
pub(crate) struct RandomWrr<T> {
    items: Vec<(u64, T)>,
    sampler: Arc<dyn RngSampler>,
}

impl<T> Wrr<T> for RandomWrr<T> {
    fn next(&self) -> Option<&T> {
        let &(total_weight, _) = self.items.last()?;
        let key = self.sampler.sample(total_weight);
        let index = self.items.partition_point(|(w, _)| *w <= key);
        Some(&self.items[index].1)
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// Returns a [`RandomWrrBuilder`] whose sampler yields `keys` in order
    /// across all [`Wrr`] instances created by the builder.
    pub(crate) fn wrr_builder(keys: &[u64]) -> RandomWrrBuilder {
        let sampler = Arc::new(MockSampler(Mutex::new(keys.to_vec().into())));
        RandomWrrBuilder::with_sampler(sampler)
    }

    /// Returns the given keys in order. Panics when the keys run out or a key
    /// is not below the total weight, so no pick depends on an unplanned key.
    #[derive(Debug)]
    struct MockSampler(Mutex<VecDeque<u64>>);

    impl RngSampler for MockSampler {
        fn sample(&self, total_weight: u64) -> u64 {
            let key = self.0.lock().unwrap().pop_front();
            let key = key.expect("test should provide a key for every weighted pick");
            assert!(
                key < total_weight,
                "key {key} should be below total weight {total_weight}"
            );
            key
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::wrr_builder;
    use super::*;

    #[test]
    fn random_wrr_partitions_weights_correctly() {
        let wrr = wrr_builder(&[0, 19, 20, 49, 50, 99]).build([("a", 20), ("b", 30), ("c", 50)]);
        for expected in ["a", "a", "b", "b", "c", "c"] {
            assert_eq!(wrr.next(), Some(&expected));
        }

        let default_wrr = RandomWrrBuilder::default().build([("a", 50)]);
        assert_eq!(default_wrr.next(), Some(&"a"));
    }

    #[test]
    fn random_wrr_skips_zero_weight_and_handles_empty() {
        let empty: RandomWrr<&str> = RandomWrrBuilder::default().build([]);
        assert_eq!(empty.next(), None);

        let all_zero = RandomWrrBuilder::default().build([("a", 0), ("b", 0)]);
        assert_eq!(all_zero.next(), None);

        let with_zero = wrr_builder(&[0]).build([("a", 0), ("b", 10), ("c", 0)]);
        assert_eq!(with_zero.next(), Some(&"b"));
    }
}
