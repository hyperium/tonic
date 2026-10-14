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

//! A mutable key/value store scoped to a single call.
//!
//! This module provides the [`CallAttributes`] type, which carries arbitrary
//! per-call data along an RPC.
//!
//! Values are keyed by their type, so each type can appear at most once. This
//! lets a component define a private newtype as its key and be sure no other
//! component will collide with it.
//!
//! # Comparison with [`Attributes`](crate::attributes::Attributes)
//!
//! [`Attributes`](crate::attributes::Attributes) is a persistent, immutable
//! map shared between components: adding to it produces a new map.
//! [`CallAttributes`] instead it is mutated in place.
//!
//! # Examples
//!
//! ```rust
//! use grpc::call_attributes::CallAttributes;
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct UserId(usize);
//!
//! let mut attrs = CallAttributes::new();
//! attrs.add(UserId(42));
//! assert_eq!(attrs.get::<UserId>(), Some(&UserId(42)));
//! ```

use std::any::TypeId;

/// A single type-erased attribute value.
type ErasedAttr = Box<dyn CloneableAny>;

/// Type-erasure trait for stored attributes, used in place of
/// [`Any`](std::any::Any) so that the concrete type's [`Clone`] impl stays
/// reachable through the trait object's vtable.
trait CloneableAny: Send + 'static {
    /// Clones `self` into a new box, preserving the concrete type.
    fn clone_boxed(&self) -> ErasedAttr;

    /// Returns the [`TypeId`] of the concrete type behind the trait object.
    fn type_id(&self) -> TypeId;
}

impl<T: Send + Clone + 'static> CloneableAny for T {
    fn clone_boxed(&self) -> ErasedAttr {
        Box::new(self.clone())
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }
}

impl dyn CloneableAny {
    /// Reinterprets this value as its concrete type `T`.
    ///
    /// A checked downcast would cost a virtual
    /// [`concrete_type_id`](CloneableAny::concrete_type_id) call on every hit.
    /// Callers here have already established the type from
    /// [`Entry::type_id`], so that call is pure overhead and is only made in
    /// debug builds.
    ///
    /// # Safety
    ///
    /// The concrete type behind `self` must be exactly `T`.
    #[inline]
    unsafe fn downcast_ref_unchecked<T: 'static>(&self) -> &T {
        debug_assert_eq!(self.type_id(), TypeId::of::<T>());
        unsafe { &*(self as *const dyn CloneableAny as *const T) }
    }

    /// Mutable counterpart of
    /// [`downcast_ref_unchecked`](Self::downcast_ref_unchecked).
    ///
    /// # Safety
    ///
    /// The concrete type behind `self` must be exactly `T`.
    #[inline]
    unsafe fn downcast_mut_unchecked<T: 'static>(&mut self) -> &mut T {
        debug_assert_eq!(self.type_id(), TypeId::of::<T>());
        unsafe { &mut *(self as *mut dyn CloneableAny as *mut T) }
    }
}

/// One stored attribute: the erased value plus the [`TypeId`] of its concrete
/// type.
///
/// Caching the `TypeId` alongside the value is what keeps lookups cheap. A
/// scan can then compare plain 128-bit integers and only pay for a virtual
/// function call on the entry it actually matches, rather than on every entry
/// it walks past.
struct Entry {
    /// `TypeId` of the concrete type stored in `value`.
    ///
    /// # Invariant
    ///
    /// Always equals the `TypeId` of the concrete type inside `value`. Every
    /// site that writes `value` writes this field from the same `T`, which is
    /// what lets [`CallAttributes::get`] treat a match here as authoritative.
    type_id: TypeId,
    value: ErasedAttr,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            type_id: self.type_id,
            value: self.value.clone_boxed(),
        }
    }
}

/// Dynamic type map storing call attributes.
///
/// Implements `Send` automatically and can be held across `.await` points or
/// transferred across threads.
///
/// Stored types must be [`Clone`], which is what makes the map itself
/// [`Clone`].
///
/// # Thread safety
///
/// This type is `Send` but **not `Sync`**: stored values are only required to
/// be [`Send`].
///
/// # Performance
///
/// At most one value is stored per type. Every operation scans the entries, so
/// they are O(n); this is intended for the handful of attributes on a typical
/// call, not for large maps.
///
/// [`Clone`] is a deep copy, so the two maps share no storage.
#[derive(Default, Clone)]
pub struct CallAttributes {
    items: Vec<Entry>,
}

impl CallAttributes {
    /// Creates a new empty `CallAttributes` collection.
    ///
    /// Allocates nothing; storage is acquired on the first insert.
    #[inline]
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Creates a new `CallAttributes` collection with room for `capacity`
    /// attributes before it needs to reallocate.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
        }
    }

    /// Inserts a value of type `T` into the call attributes.
    ///
    /// If an entry of type `T` already exists, it is replaced and the previous
    /// value's destructor is executed immediately.
    ///
    /// Runs in O(n), since it scans for an existing entry of type `T`.
    ///
    /// # Examples
    ///
    /// Basic usage
    ///
    /// ```
    /// # use grpc::call_attributes::CallAttributes;
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct UserId(usize);
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct TraceId(&'static str);
    ///
    /// let mut attrs = CallAttributes::new();
    ///
    /// // Values are keyed by their type, so unrelated types coexist.
    /// attrs.add(UserId(42));
    /// attrs.add(TraceId("abc"));
    ///
    /// assert_eq!(attrs.get::<UserId>(), Some(&UserId(42)));
    /// assert_eq!(attrs.get::<TraceId>(), Some(&TraceId("abc")));
    ///
    /// // Adding a second value of the same type replaces the first one.
    /// attrs.add(UserId(7));
    ///
    /// assert_eq!(attrs.get::<UserId>(), Some(&UserId(7)));
    /// ```
    pub fn add<T: 'static + Send + Clone>(&mut self, val: T) {
        let type_id = TypeId::of::<T>();
        for entry in &mut self.items {
            if entry.type_id == type_id {
                // Assigning drops the old box, running the previous value's
                // destructor. `type_id` already matches, so it stays correct.
                entry.value = Box::new(val);
                return;
            }
        }
        self.items.push(Entry {
            type_id,
            value: Box::new(val),
        });
    }

    /// Retrieves an immutable reference to the value of type `T`, if present.
    ///
    /// # Examples
    ///
    /// Basic usage
    ///
    /// ```
    /// # use grpc::call_attributes::CallAttributes;
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct UserId(usize);
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct RetryCount(usize);
    ///
    /// let mut attrs = CallAttributes::new();
    /// attrs.add(UserId(42));
    ///
    /// assert_eq!(attrs.get::<UserId>(), Some(&UserId(42)));
    ///
    /// // Entries are looked up by type; a type that was never added is
    /// // absent, even though it wraps the same inner type as `UserId`.
    /// assert_eq!(attrs.get::<RetryCount>(), None);
    /// ```
    pub fn get<T: 'static>(&self) -> Option<&T> {
        let type_id = TypeId::of::<T>();
        for entry in &self.items {
            if entry.type_id == type_id {
                // SAFETY: `Entry::type_id`'s invariant says this entry's value
                // has concrete type `T`, which the comparison just confirmed.
                return Some(unsafe { entry.value.downcast_ref_unchecked::<T>() });
            }
        }
        None
    }

    /// Retrieves a mutable reference to the value of type `T`, if present.
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        let type_id = TypeId::of::<T>();
        for entry in &mut self.items {
            if entry.type_id == type_id {
                // SAFETY: see `get`.
                return Some(unsafe { entry.value.downcast_mut_unchecked::<T>() });
            }
        }
        None
    }

    /// Clears all stored attributes, executing their destructors.
    ///
    /// Retains the allocated capacity, so a cleared collection can be refilled
    /// without reallocating.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct UserId(usize);

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct TraceId(&'static str);

    /// Shared counter of executed destructors, for tests that only care *how
    /// many* values were dropped.
    #[derive(Clone, Default)]
    struct DropCount(Arc<AtomicUsize>);

    impl DropCount {
        fn new() -> Self {
            Self::default()
        }

        /// A value that increments this counter when dropped.
        fn tracked(&self) -> Tracked {
            Tracked(self.0.clone())
        }

        /// How many tracked values have been dropped so far.
        fn get(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Increments the [`DropCount`] it came from when dropped.
    #[derive(Clone)]
    struct Tracked(Arc<AtomicUsize>);

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn insert_and_get() {
        let mut attrs = CallAttributes::new();

        attrs.add(UserId(42));
        attrs.add(TraceId("trace-1"));

        assert_eq!(attrs.get::<UserId>(), Some(&UserId(42)));
        assert_eq!(attrs.get::<TraceId>(), Some(&TraceId("trace-1")));
        assert_eq!(attrs.get::<String>(), None);
    }

    #[test]
    fn overwrite_value() {
        let mut attrs = CallAttributes::new();

        attrs.add(UserId(1));
        assert_eq!(attrs.get::<UserId>(), Some(&UserId(1)));

        // Overwrite existing TypeId
        attrs.add(UserId(2));
        assert_eq!(attrs.get::<UserId>(), Some(&UserId(2)));
    }

    #[test]
    fn get_mut() {
        let mut attrs = CallAttributes::new();
        attrs.add(UserId(10));

        if let Some(uid) = attrs.get_mut::<UserId>() {
            uid.0 = 20;
        }

        assert_eq!(attrs.get::<UserId>(), Some(&UserId(20)));
    }

    #[test]
    fn get_mut_absent_type() {
        let mut attrs = CallAttributes::new();
        attrs.add(UserId(1));
        assert!(attrs.get_mut::<TraceId>().is_none());
    }

    #[test]
    fn many_distinct_types() {
        // Enough entries to force the backing `Vec` to reallocate at least once
        // from a zero-capacity start, so lookups are exercised against storage
        // that has been moved.
        let mut attrs = CallAttributes::new();

        attrs.add(10u32);
        attrs.add(20u64);
        attrs.add("third".to_string());
        attrs.add(true);
        attrs.add(1.5f64);

        assert_eq!(attrs.get::<u32>(), Some(&10));
        assert_eq!(attrs.get::<u64>(), Some(&20));
        assert_eq!(attrs.get::<String>(), Some(&"third".to_string()));
        assert_eq!(attrs.get::<bool>(), Some(&true));
        assert_eq!(attrs.get::<f64>(), Some(&1.5));
        assert_eq!(attrs.get::<i32>(), None);
    }

    #[test]
    fn call_attributes_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CallAttributes>();
    }

    #[test]
    fn clear_runs_destructors_and_empties() {
        let drops = DropCount::new();
        let mut attrs = CallAttributes::new();
        attrs.add(drops.tracked());
        attrs.add(UserId(1));
        attrs.add(TraceId("t"));

        attrs.clear();

        assert_eq!(drops.get(), 1, "clear must run destructors");
        assert_eq!(attrs.get::<UserId>(), None);
        assert_eq!(attrs.get::<TraceId>(), None);

        // The collection stays usable after `clear`.
        attrs.add(UserId(2));
        assert_eq!(attrs.get::<UserId>(), Some(&UserId(2)));
    }

    #[test]
    fn overwrite_entry_not_at_front() {
        // The overwritten entry is neither the first nor the last, so this
        // covers the mid-scan match rather than a boundary.
        let mut attrs = CallAttributes::new();
        attrs.add(1u32);
        attrs.add(2u64);
        attrs.add(3i8);
        attrs.add(4i16);
        attrs.add(5i64);

        attrs.add(100u64);

        assert_eq!(attrs.get::<u64>(), Some(&100));
        assert_eq!(attrs.get::<u32>(), Some(&1));
        assert_eq!(attrs.get::<i64>(), Some(&5));
    }

    #[test]
    fn overwrite_does_not_grow_storage() {
        // Repeated inserts of the same type must reuse the existing entry
        // rather than appending, otherwise the map grows without bound and
        // `get` starts returning a stale value.
        let mut attrs = CallAttributes::new();
        attrs.add(0u64);

        for i in 1..1000u64 {
            attrs.add(i);
        }

        assert_eq!(attrs.get::<u64>(), Some(&999));
        assert_eq!(attrs.items.len(), 1, "overwrite must reuse the entry");
    }

    #[test]
    fn clone_is_deep_and_independent() {
        let mut attrs = CallAttributes::new();
        attrs.add(UserId(1));
        attrs.add(TraceId("t"));
        attrs.add("s".to_string());

        let mut cloned = attrs.clone();
        assert_eq!(cloned.get::<UserId>(), Some(&UserId(1)));
        assert_eq!(cloned.get::<TraceId>(), Some(&TraceId("t")));
        assert_eq!(cloned.get::<String>(), Some(&"s".to_string()));

        // Independent storage.
        assert_ne!(
            attrs.get::<String>().unwrap() as *const String,
            cloned.get::<String>().unwrap() as *const String
        );

        cloned.add(UserId(2));
        assert_eq!(attrs.get::<UserId>(), Some(&UserId(1)));
        assert_eq!(cloned.get::<UserId>(), Some(&UserId(2)));

        // Clone stays usable and keeps deduplicating.
        cloned.add(UserId(3));
        assert_eq!(cloned.get::<UserId>(), Some(&UserId(3)));
    }

    #[test]
    fn clone_panic_drops_already_cloned_values() {
        // Pins the panic-safety of `Clone`: cloned entries are owned by the new
        // map as they are produced, so a panicking `T::clone` must leave the
        // partial copy to be torn down by unwinding rather than leaked.
        struct Boom;
        impl Clone for Boom {
            fn clone(&self) -> Self {
                panic!("clone blew up")
            }
        }

        let drops = DropCount::new();
        let mut attrs = CallAttributes::new();
        attrs.add(drops.tracked());
        attrs.add(Boom);

        // Entries are cloned in insertion order, so the partial clone owns one
        // live value when `Boom` panics.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| attrs.clone()));

        assert!(result.is_err(), "clone must propagate the panic");
        assert_eq!(
            drops.get(),
            1,
            "the already-cloned value must be dropped while unwinding"
        );

        // The source is untouched and still usable.
        assert!(attrs.get::<Tracked>().is_some());
        drop(attrs);
        assert_eq!(drops.get(), 2);
    }
}
