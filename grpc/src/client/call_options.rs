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

use std::any::TypeId;
use std::time::Instant;

/// Settings to configure RPCs sent using the [`Invoke`](super::Invoke) trait.
///
/// Besides the built-in settings (e.g. the deadline), `CallOptions` carries
/// custom options keyed by type, set with [`set_option`](Self::set_option)
/// and read with [`get_option`](Self::get_option). Each type can appear at
/// most once.
///
/// Most applications will not need this type, and will set options via the
/// generated (e.g. protobuf) APIs instead.
///
/// # Thread safety
///
/// This type is `Send` but **not `Sync`**: custom option values are only
/// required to be [`Send`].
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct CallOptions {
    /// The deadline for the call.  If unset, the call may run indefinitely.
    deadline: Option<Instant>,
    /// Custom options keyed by type.  Holds at most one entry per type.
    options: Vec<Entry>,
}

impl CallOptions {
    /// Constructs a new [`CallOptions`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Constructs a new [`CallOptions`] with the default settings and room
    /// for at least `capacity` custom options before it needs to reallocate.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            options: Vec::with_capacity(capacity),
            ..Self::default()
        }
    }

    /// Applies a deadline to the call; when the deadline is reached, the call
    /// is cancelled.
    pub fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
    }

    /// Reads the deadline currently set in the [`CallOptions`].
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Sets a custom option of type `T`.
    ///
    /// If an option of type `T` is already set, it is replaced and the
    /// previous value's destructor is executed immediately.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::client::CallOptions;
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct UserId(usize);
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct TraceId(&'static str);
    ///
    /// let mut opts = CallOptions::new();
    ///
    /// // Options are keyed by their type, so unrelated types coexist.
    /// opts.set_option(UserId(42));
    /// opts.set_option(TraceId("abc"));
    ///
    /// assert_eq!(opts.get_option::<UserId>(), Some(&UserId(42)));
    /// assert_eq!(opts.get_option::<TraceId>(), Some(&TraceId("abc")));
    ///
    /// // Setting a second value of the same type replaces the first one.
    /// opts.set_option(UserId(7));
    ///
    /// assert_eq!(opts.get_option::<UserId>(), Some(&UserId(7)));
    /// ```
    pub fn set_option<T: Clone + Send + 'static>(&mut self, value: T) {
        let type_id = TypeId::of::<T>();
        for entry in &mut self.options {
            if entry.type_id == type_id {
                // Assigning drops the old box, running the previous value's
                // destructor. `type_id` already matches, so it stays correct.
                entry.value = Box::new(value);
                return;
            }
        }
        self.options.push(Entry {
            type_id,
            value: Box::new(value),
        });
    }

    /// Returns a reference to the custom option of type `T`, if one is set.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grpc::client::CallOptions;
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct UserId(usize);
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct RetryCount(usize);
    ///
    /// let mut opts = CallOptions::new();
    /// opts.set_option(UserId(42));
    ///
    /// assert_eq!(opts.get_option::<UserId>(), Some(&UserId(42)));
    ///
    /// // Options are looked up by type; a type that was never set is absent,
    /// // even though it wraps the same inner type as `UserId`.
    /// assert_eq!(opts.get_option::<RetryCount>(), None);
    /// ```
    pub fn get_option<T: 'static>(&self) -> Option<&T> {
        let type_id = TypeId::of::<T>();
        for entry in &self.options {
            if entry.type_id == type_id {
                // SAFETY: `Entry::type_id`'s invariant says this entry's value
                // has concrete type `T`, which the comparison just confirmed.
                return Some(unsafe { entry.value.downcast_ref_unchecked::<T>() });
            }
        }
        None
    }
}

/// A single type-erased option value.
type ErasedOption = Box<dyn CloneableAny>;

/// Type-erasure trait for stored options, used in place of
/// [`Any`](std::any::Any) so that the concrete type's [`Clone`] impl stays
/// reachable through the trait object's vtable.
trait CloneableAny: Send + 'static {
    /// Clones `self` into a new box, preserving the concrete type.
    fn clone_boxed(&self) -> ErasedOption;

    /// Returns the [`TypeId`] of the concrete type behind the trait object.
    fn type_id(&self) -> TypeId;
}

impl<T: Send + Clone + 'static> CloneableAny for T {
    fn clone_boxed(&self) -> ErasedOption {
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
    /// [`type_id`](CloneableAny::type_id) call on every hit. Callers here have
    /// already established the type from [`Entry::type_id`], so that call is
    /// pure overhead and is only made in debug builds.
    ///
    /// # Safety
    ///
    /// The concrete type behind `self` must be exactly `T`.
    #[inline]
    unsafe fn downcast_ref_unchecked<T: 'static>(&self) -> &T {
        debug_assert_eq!(self.type_id(), TypeId::of::<T>());
        unsafe { &*(self as *const dyn CloneableAny as *const T) }
    }
}

/// One stored option: the erased value plus the [`TypeId`] of its concrete
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
    /// what lets [`CallOptions::get_option`] treat a match here as
    /// authoritative.
    type_id: TypeId,
    value: ErasedOption,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            type_id: self.type_id,
            value: self.value.clone_boxed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;
    use crate::rt::GrpcRuntime;

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
    fn set_and_get() {
        let mut opts = CallOptions::new();

        opts.set_option(UserId(42));
        opts.set_option(TraceId("trace-1"));

        assert_eq!(opts.get_option::<UserId>(), Some(&UserId(42)));
        assert_eq!(opts.get_option::<TraceId>(), Some(&TraceId("trace-1")));
        assert_eq!(opts.get_option::<String>(), None);
    }

    #[test]
    fn overwrite_value() {
        let mut opts = CallOptions::new();

        opts.set_option(UserId(1));
        assert_eq!(opts.get_option::<UserId>(), Some(&UserId(1)));

        // Overwrite existing TypeId
        opts.set_option(UserId(2));
        assert_eq!(opts.get_option::<UserId>(), Some(&UserId(2)));
    }

    #[test]
    fn set_option_drops_replaced_value() {
        let drops = DropCount::new();
        let mut opts = CallOptions::new();
        opts.set_option(drops.tracked());
        assert_eq!(drops.get(), 0);

        opts.set_option(drops.tracked());
        assert_eq!(drops.get(), 1, "replacing must drop the previous value");

        drop(opts);
        assert_eq!(drops.get(), 2, "dropping must drop the stored value");
    }

    #[test]
    fn many_distinct_types() {
        // Enough entries to force the backing `Vec` to reallocate at least once
        // from a zero-capacity start, so lookups are exercised against storage
        // that has been moved.
        let mut opts = CallOptions::new();

        opts.set_option(10u32);
        opts.set_option(20u64);
        opts.set_option("third".to_string());
        opts.set_option(true);
        opts.set_option(1.5f64);

        assert_eq!(opts.get_option::<u32>(), Some(&10));
        assert_eq!(opts.get_option::<u64>(), Some(&20));
        assert_eq!(opts.get_option::<String>(), Some(&"third".to_string()));
        assert_eq!(opts.get_option::<bool>(), Some(&true));
        assert_eq!(opts.get_option::<f64>(), Some(&1.5));
        assert_eq!(opts.get_option::<i32>(), None);
    }

    #[test]
    fn call_options_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CallOptions>();
    }

    #[test]
    fn overwrite_entry_not_at_front() {
        // The overwritten entry is neither the first nor the last, so this
        // covers the mid-scan match rather than a boundary.
        let mut opts = CallOptions::new();
        opts.set_option(1u32);
        opts.set_option(2u64);
        opts.set_option(3i8);
        opts.set_option(4i16);
        opts.set_option(5i64);

        opts.set_option(100u64);

        assert_eq!(opts.get_option::<u64>(), Some(&100));
        assert_eq!(opts.get_option::<u32>(), Some(&1));
        assert_eq!(opts.get_option::<i64>(), Some(&5));
    }

    #[test]
    fn overwrite_does_not_grow_storage() {
        // Repeated sets of the same type must reuse the existing entry rather
        // than appending, otherwise the storage grows without bound and
        // `get_option` starts returning a stale value.
        let mut opts = CallOptions::new();
        opts.set_option(0u64);

        for i in 1..1000u64 {
            opts.set_option(i);
        }

        assert_eq!(opts.get_option::<u64>(), Some(&999));
        assert_eq!(opts.options.len(), 1, "overwrite must reuse the entry");
    }

    #[test]
    fn clone_is_deep_and_independent() {
        let mut opts = CallOptions::new();
        opts.set_option(UserId(1));
        opts.set_option(TraceId("t"));
        opts.set_option("s".to_string());

        let mut cloned = opts.clone();
        assert_eq!(cloned.get_option::<UserId>(), Some(&UserId(1)));
        assert_eq!(cloned.get_option::<TraceId>(), Some(&TraceId("t")));
        assert_eq!(cloned.get_option::<String>(), Some(&"s".to_string()));

        // Independent storage.
        assert_ne!(
            opts.get_option::<String>().unwrap() as *const String,
            cloned.get_option::<String>().unwrap() as *const String
        );

        cloned.set_option(UserId(2));
        assert_eq!(opts.get_option::<UserId>(), Some(&UserId(1)));
        assert_eq!(cloned.get_option::<UserId>(), Some(&UserId(2)));

        // Clone stays usable and keeps deduplicating.
        cloned.set_option(UserId(3));
        assert_eq!(cloned.get_option::<UserId>(), Some(&UserId(3)));
    }

    #[test]
    fn clone_preserves_deadline() {
        let deadline = GrpcRuntime::default().now() + Duration::from_secs(10);
        let mut opts = CallOptions::new();
        opts.set_deadline(deadline);
        opts.set_option(UserId(1));

        let cloned = opts.clone();
        assert_eq!(cloned.deadline(), Some(deadline));
        assert_eq!(cloned.get_option::<UserId>(), Some(&UserId(1)));
    }

    #[test]
    fn clone_panic_drops_already_cloned_values() {
        // Pins the panic-safety of `Clone`: cloned entries are owned by the new
        // value as they are produced, so a panicking `T::clone` must leave the
        // partial copy to be torn down by unwinding rather than leaked.
        struct Boom;
        impl Clone for Boom {
            fn clone(&self) -> Self {
                panic!("clone blew up")
            }
        }

        let drops = DropCount::new();
        let mut opts = CallOptions::new();
        opts.set_option(drops.tracked());
        opts.set_option(Boom);

        // Entries are cloned in insertion order, so the partial clone owns one
        // live value when `Boom` panics.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| opts.clone()));

        assert!(result.is_err(), "clone must propagate the panic");
        assert_eq!(
            drops.get(),
            1,
            "the already-cloned value must be dropped while unwinding"
        );

        // The source is untouched and still usable.
        assert!(opts.get_option::<Tracked>().is_some());
        drop(opts);
        assert_eq!(drops.get(), 2);
    }
}
