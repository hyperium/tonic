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
use std::sync::Arc;

use crate::attributes::avl::Avl;

mod avl;

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
            // Fallback for safety, though Avl structure guarantees same-type
            // comparison.
            TypeId::of::<T>().cmp(&other.any_ref().type_id())
        }
    }
}

#[derive(Clone, Debug)]
struct AttributeValue(Arc<dyn AttributeTrait>);

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
#[derive(Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Attributes {
    map: Avl<TypeId, AttributeValue>,
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
            map: self.map.add(id, AttributeValue(Arc::new(value))),
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
            map: self.map.remove(&id),
        }
    }

    /// Inserts all values from another Attributes object into this one.
    /// Returns a new Attributes object with the values added.
    /// If a value of the same type already exists, it is replaced by the value
    /// from `other`.
    pub fn union(&self, other: &Attributes) -> Self {
        let mut new_map = self.map.clone();
        for (k, v) in other.map.iter() {
            new_map = new_map.add(*k, v.clone());
        }
        Attributes { map: new_map }
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
    fn test_union() {
        let a1 = Attributes::new().add(10i32).add(20u32);
        let a2 = Attributes::new().add(30i64).add(40i32); // 40i32 should overwrite 10i32

        let a3 = a1.union(&a2);

        assert_eq!(a3.get::<i32>(), Some(&40));
        assert_eq!(a3.get::<u32>(), Some(&20));
        assert_eq!(a3.get::<i64>(), Some(&30));

        // Original maps should be unchanged
        assert_eq!(a1.get::<i32>(), Some(&10));
        assert_eq!(a2.get::<i32>(), Some(&40));
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
}
