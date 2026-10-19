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

//! A generic key/value store.
//!
//! This module provides the [`Attributes`] type, used by many different
//! components of gRPC including plugins like name resolvers, load balancers,
//! and credentials, and stores arbitrary configuration data or state.
//!
//! # Examples
//!
//! ```rust
//! use grpc::attributes::Attributes;
//!
//! #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
//! struct MyConfig(i32);
//!
//! let attrs = Attributes::new().add(MyConfig(42));
//! assert_eq!(attrs.get::<MyConfig>(), Some(&MyConfig(42)));
//!
//! let attrs = attrs.remove::<MyConfig>();
//! assert_eq!(attrs.get::<MyConfig>(), None);
//! ```

use std::any::Any;
use std::any::TypeId;
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::sync::Arc;

/// Ensures only types that support comparison can be inserted into the
/// Attributes struct. This allows the use of value-based equality rather than
/// relying on pointer comparisons.
trait AttributeTrait: Any + Send + Sync + Debug {
    fn any_ref(&self) -> &dyn Any;
    fn dyn_eq(&self, other: &dyn AttributeTrait) -> bool;
}

impl<T: Any + Send + Sync + Eq + Debug> AttributeTrait for T {
    fn any_ref(&self) -> &dyn Any {
        self
    }

    fn dyn_eq(&self, other: &dyn AttributeTrait) -> bool {
        if let Some(other) = other.any_ref().downcast_ref::<T>() {
            self == other
        } else {
            false
        }
    }
}

/// A node in the persistent list backing [`Attributes`].
///
/// Each node points to the next (older) node, allowing structural sharing
/// between `Attributes` instances.
struct Node {
    value: Arc<dyn AttributeTrait>,
    next: Option<Arc<Node>>,
}

/// A collection of attributes indexed by their type.
///
/// `Attributes` provides a map-like interface where values are keyed by their
/// TypeId.
///
/// Equality and ordering of `Attributes` are structural.
/// This means two `Attributes` maps are equal if they contain the same set of
/// values, compared by value (via `Eq` trait).
/// Stored types must implement `Any + Send + Sync + Eq + Debug`.
///
/// # Warning
///
/// This collection is intended to store a small number of values (few hundreds)
/// and is optimized for memory usage. It is **not** optimized for query speed.
#[derive(Clone, Default)]
pub struct Attributes {
    head: Option<Arc<Node>>,
}

impl Attributes {
    /// Constructs a new, empty instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value to the attributes.
    /// Returns a new Attributes object with the value added.
    /// If a value of the same type already exists, it is replaced.
    pub fn add<T: Send + Sync + Eq + Debug + 'static>(&self, value: T) -> Self {
        Attributes {
            head: Some(Arc::new(Node {
                value: Arc::new(value),
                next: self.head.clone(),
            })),
        }
    }

    /// Gets a reference to a value of type T.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        let mut current = self.head.as_deref();
        while let Some(node) = current {
            if let Some(v) = node.value.any_ref().downcast_ref::<T>() {
                return Some(v);
            }
            current = node.next.as_deref();
        }
        None
    }

    /// Removes the value of type T, if present.
    /// Returns a new Attributes object without the value.
    ///
    /// The returned object holds no reference to the removed value, so it is
    /// dropped once no other `Attributes` instance references it.
    ///
    /// This operation is O(n) in both time and space.
    pub fn remove<T: 'static>(&self) -> Self {
        // Nodes older than the oldest `T` are shared as-is; newer nodes are
        // rebuilt (reusing their value `Arc`s) with every `T` left out.
        let mut original_nodes = Vec::new();
        let mut current = self.head.as_ref();
        while let Some(node) = current {
            original_nodes.push(node);
            current = node.next.as_ref();
        }
        let Some(oldest) = original_nodes
            .iter()
            .rposition(|n| n.value.any_ref().is::<T>())
        else {
            return self.clone();
        };
        let tail = original_nodes[oldest].next.clone();
        let head = original_nodes[..oldest]
            .iter()
            .rev()
            .filter(|n| !n.value.any_ref().is::<T>())
            .fold(tail, |next, original| {
                Some(Arc::new(Node {
                    value: original.value.clone(),
                    next,
                }))
            });
        Attributes { head }
    }

    /// Returns an iterator over the live values, newest first.
    ///
    /// Each type is yielded at most once; values shadowed by a more recent
    /// `add` of the same type are skipped.
    fn iter(&self) -> impl Iterator<Item = &Arc<dyn AttributeTrait>> {
        let mut current = self.head.as_deref();
        let mut seen = BTreeSet::new();
        std::iter::from_fn(move || {
            while let Some(node) = current {
                current = node.next.as_deref();
                if seen.insert(node.value.any_ref().type_id()) {
                    return Some(&node.value);
                }
            }
            None
        })
    }
}

impl Debug for Attributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl PartialEq for Attributes {
    fn eq(&self, other: &Self) -> bool {
        fn collect(a: &Attributes) -> Vec<(TypeId, &Arc<dyn AttributeTrait>)> {
            let mut v: Vec<_> = a.iter().map(|v| (v.any_ref().type_id(), v)).collect();
            v.sort_by_key(|(id, _)| *id);
            v
        }
        let (v1, v2) = (collect(self), collect(other));
        v1.len() == v2.len()
            && v1
                .iter()
                .zip(&v2)
                .all(|((id1, a), (id2, b))| id1 == id2 && a.dyn_eq(b.as_ref()))
    }
}

impl Eq for Attributes {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eq() {
        let a1 = Attributes::new().add(10i32);
        let a2 = a1.clone();
        let a3 = Attributes::new().add(10i32); // Structural equality

        assert_eq!(a1, a2);
        assert_eq!(a1, a3); // Now equal because 10 == 10

        let a4 = Attributes::new().add(10i32).add("foo".to_string());
        assert_ne!(a1, a4);
    }

    #[test]
    fn test_attributes() {
        let attrs = Attributes::new();
        let attrs = attrs.add(42i32);
        let attrs = attrs.add("hello".to_string());

        assert_eq!(attrs.get::<i32>(), Some(&42));
        assert_eq!(attrs.get::<String>(), Some(&"hello".to_string()));
        assert_eq!(attrs.get::<bool>(), None);
    }

    #[test]
    fn test_persistence() {
        let a1 = Attributes::new().add(10i32);
        let a2 = a1.add(20u32);

        assert_eq!(a1.get::<i32>(), Some(&10));
        assert_eq!(a1.get::<u32>(), None);

        assert_eq!(a2.get::<i32>(), Some(&10));
        assert_eq!(a2.get::<u32>(), Some(&20));
    }

    #[test]
    fn test_overwrite() {
        let a1 = Attributes::new().add(10i32);
        let a2 = a1.add(20i32);

        assert_eq!(a1.get::<i32>(), Some(&10));
        assert_eq!(a2.get::<i32>(), Some(&20));
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Priority {
        weight: u64,
        name: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Config {
        retries: u32,
        timeout_ms: u64,
    }

    #[test]
    fn test_custom_structs() {
        let p = Priority {
            weight: 123,
            name: "alice".into(),
        };
        let config = Config {
            retries: 3,
            timeout_ms: 1000,
        };

        let attrs = Attributes::new().add(p.clone()).add(config.clone());

        assert_eq!(attrs.get::<Priority>(), Some(&p));
        assert_eq!(attrs.get::<Config>(), Some(&config));

        // Test overwrite
        let p2 = Priority {
            weight: 456,
            name: "bob".into(),
        };
        let attrs2 = attrs.add(p2.clone());

        assert_eq!(attrs2.get::<Priority>(), Some(&p2));
        assert_eq!(attrs2.get::<Config>(), Some(&config));

        // original should be unchanged
        assert_eq!(attrs.get::<Priority>(), Some(&p));
    }

    fn type_ids(attrs: &Attributes) -> Vec<TypeId> {
        attrs.iter().map(|v| v.any_ref().type_id()).collect()
    }

    #[test]
    fn test_iter_order() {
        let attrs = Attributes::new().add(1i32).add(2u32).add(3u64);
        assert_eq!(
            type_ids(&attrs),
            vec![
                TypeId::of::<u64>(),
                TypeId::of::<u32>(),
                TypeId::of::<i32>()
            ]
        );
    }

    #[test]
    fn test_iter_shadowing() {
        let attrs = Attributes::new().add(1i32).add(2i32);
        let values: Vec<_> = attrs
            .iter()
            .map(|v| *v.any_ref().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![2]);
    }

    #[test]
    fn test_eq_ignores_shadowed() {
        assert_eq!(
            Attributes::new().add(1i32).add(2i32),
            Attributes::new().add(2i32)
        );
    }

    #[test]
    fn test_eq_order_independent() {
        assert_eq!(
            Attributes::new().add(1i32).add(2u32),
            Attributes::new().add(2u32).add(1i32)
        );
        assert_ne!(
            Attributes::new().add(1i32).add(2u32),
            Attributes::new().add(2u32).add(3i32)
        );
    }

    #[test]
    fn test_structural_sharing() {
        let a1 = Attributes::new().add(1i32);
        let a2 = a1.add(2u32);
        let a1_head = a1.head.as_ref().unwrap();
        let a2_next = a2.head.as_ref().unwrap().next.as_ref().unwrap();
        assert!(Arc::ptr_eq(a1_head, a2_next));
    }

    #[test]
    fn test_debug_live_values() {
        let attrs = Attributes::new().add(1i32).add(42i32).add("hi".to_string());
        assert_eq!(format!("{attrs:?}"), r#"["hi", 42]"#);
        assert_eq!(format!("{:?}", Attributes::new()), "[]");
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Attributes>();
    }

    /// Returns the `n`th node (0 = head) of the raw chain.
    fn nth_node(attrs: &Attributes, n: usize) -> &Arc<Node> {
        let mut node = attrs.head.as_ref().unwrap();
        for _ in 0..n {
            node = node.next.as_ref().unwrap();
        }
        node
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Tracker(Arc<()>);

    #[test]
    fn test_remove() {
        let attrs = Attributes::new()
            .add(1i32)
            .add("s".to_string())
            .remove::<i32>();
        assert_eq!(attrs.get::<i32>(), None);
        assert_eq!(attrs.get::<String>(), Some(&"s".to_string()));
    }

    #[test]
    fn test_remove_missing() {
        let a1 = Attributes::new().add(1i32);
        let a2 = a1.remove::<bool>();
        assert_eq!(a1, a2);
        assert!(Arc::ptr_eq(nth_node(&a1, 0), nth_node(&a2, 0)));
        assert!(Attributes::new().remove::<bool>().head.is_none());
    }

    #[test]
    fn test_remove_persistence() {
        let a1 = Attributes::new().add(1i32).add(2u32);
        let a2 = a1.remove::<i32>();
        assert_eq!(a1.get::<i32>(), Some(&1));
        assert_eq!(a1.get::<u32>(), Some(&2));
        assert_eq!(a2.get::<i32>(), None);
        assert_eq!(a2.get::<u32>(), Some(&2));
    }

    #[test]
    fn test_remove_preserves_order() {
        let attrs = Attributes::new()
            .add(1u8)
            .add(2i32)
            .add(3u32)
            .add(4u64)
            .remove::<i32>();
        assert_eq!(
            type_ids(&attrs),
            vec![TypeId::of::<u64>(), TypeId::of::<u32>(), TypeId::of::<u8>()]
        );
    }

    #[test]
    fn test_remove_shares_tail() {
        let a1 = Attributes::new().add(1u8).add(2i32).add(3u32).add(4u64);
        let a2 = a1.remove::<i32>();
        // u64 and u32 nodes are rebuilt; the u8 node is shared.
        assert!(!Arc::ptr_eq(nth_node(&a1, 0), nth_node(&a2, 0)));
        assert!(Arc::ptr_eq(nth_node(&a1, 3), nth_node(&a2, 2)));
        assert!(nth_node(&a2, 2).next.is_none());
        // Rebuilt nodes reuse the value `Arc`s.
        assert!(Arc::ptr_eq(
            &nth_node(&a1, 0).value,
            &nth_node(&a2, 0).value
        ));
    }

    #[test]
    fn test_remove_head_shares_rest() {
        let a1 = Attributes::new().add(1u8).add(2i32);
        let a2 = a1.remove::<i32>();
        assert!(Arc::ptr_eq(nth_node(&a1, 1), nth_node(&a2, 0)));
    }

    #[test]
    fn test_remove_drops_value() {
        let probe = Arc::new(());
        let a1 = Attributes::new().add(Tracker(probe.clone())).add(1u8);
        let a2 = a1.remove::<Tracker>();
        assert_eq!(Arc::strong_count(&probe), 2);
        drop(a1);
        assert_eq!(Arc::strong_count(&probe), 1);
        assert_eq!(a2.get::<u8>(), Some(&1));
    }

    #[test]
    fn test_remove_shadowed() {
        let probe = Arc::new(());
        let a1 = Attributes::new()
            .add(Tracker(probe.clone()))
            .add(1u8)
            .add(Tracker(probe.clone()));
        let a2 = a1.remove::<Tracker>();
        assert_eq!(Arc::strong_count(&probe), 3);
        drop(a1);
        assert_eq!(Arc::strong_count(&probe), 1);
        assert_eq!(a2.get::<Tracker>(), None);
        assert_eq!(a2.get::<u8>(), Some(&1));
    }
}
