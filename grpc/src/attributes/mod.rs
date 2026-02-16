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

use std::any::{Any, TypeId};
use std::cmp::Ordering;
use std::fmt::Debug;

use crate::attributes::linked_list::LinkedList;

mod linked_list;

/// Ensures only types that support comparison can be inserted into the
/// Attributes struct. This allows the use of value-based equality rather than
/// relying on pointer comparisons.
trait AttributeTrait: Any + Send + Sync + Debug {
    fn any_ref(&self) -> &dyn Any;
    fn dyn_eq(&self, other: &dyn AttributeTrait) -> bool;
    fn dyn_cmp(&self, other: &dyn AttributeTrait) -> Ordering;
}

impl<T: Any + Send + Sync + Eq + Ord + Debug> AttributeTrait for T {
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

    fn dyn_cmp(&self, other: &dyn AttributeTrait) -> Ordering {
        if let Some(other) = other.any_ref().downcast_ref::<T>() {
            self.cmp(other)
        } else {
            // Fallback for safety, though map structure guarantees same-type
            // comparison.
            TypeId::of::<T>().cmp(&other.any_ref().type_id())
        }
    }
}

#[derive(Debug)]
struct AttributeValue(Box<dyn AttributeTrait>);

impl PartialEq for AttributeValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.dyn_eq(other.0.as_ref())
    }
}

impl Eq for AttributeValue {}

impl PartialOrd for AttributeValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AttributeValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.dyn_cmp(other.0.as_ref())
    }
}

/// A collection of attributes indexed by their type.
///
/// `Attributes` provides a map-like interface where values are keyed by their
/// TypeId.
///
/// Equality and ordering of `Attributes` are structural.
/// This means two `Attributes` maps are equal if they contain the same set of
/// values, compared by value (via `Eq` trait).
/// Stored types must implement `Any + Send + Sync + Eq + Ord + Debug`.
///
/// # Warning
///
/// This collection is intended to store a small number of values (few hundreds)
/// and is optimized for memory usage. It is **not** optimized for query speed.
#[derive(Clone, Default, Debug)]
pub struct Attributes {
    map: LinkedList<TypeId, AttributeValue>,
}

impl Attributes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value to the attributes.
    /// Returns a new Attributes object with the value added.
    /// If a value of the same type already exists, it is replaced.
    pub fn add<T: Send + Sync + Eq + Ord + Debug + 'static>(&self, value: T) -> Self {
        let id = TypeId::of::<T>();
        Attributes {
            map: self.map.add(id, AttributeValue(Box::new(value))),
        }
    }

    /// Gets a reference to a value of type T.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        let id = TypeId::of::<T>();
        self.map.get(&id).and_then(|v| v.0.any_ref().downcast_ref())
    }

    /// Removes a value of type T from the attributes.
    /// Returns a new Attributes object with the value removed.
    pub fn remove<T: 'static>(&self) -> Self {
        let id = TypeId::of::<T>();
        Attributes {
            map: self.map.remove(id),
        }
    }
}

impl PartialEq for Attributes {
    fn eq(&self, other: &Self) -> bool {
        let mut v1: Vec<_> = self.map.iter().collect();
        let mut v2: Vec<_> = other.map.iter().collect();
        v1.sort();
        v2.sort();
        v1 == v2
    }
}

impl Eq for Attributes {}

impl PartialOrd for Attributes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Attributes {
    fn cmp(&self, other: &Self) -> Ordering {
        let mut v1: Vec<_> = self.map.iter().collect();
        let mut v2: Vec<_> = other.map.iter().collect();
        v1.sort();
        v2.sort();
        v1.cmp(&v2)
    }
}

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
    fn test_remove() {
        let attrs = Attributes::new().add(10i32).add(20u32);
        let attrs2 = attrs.remove::<i32>();

        assert_eq!(attrs.get::<i32>(), Some(&10));
        assert_eq!(attrs.get::<u32>(), Some(&20));

        assert_eq!(attrs2.get::<i32>(), None);
        assert_eq!(attrs2.get::<u32>(), Some(&20));
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

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Priority {
        weight: u64,
        name: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
}
