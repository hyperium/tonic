mod extensions;
mod task_local_context;

pub use extensions::{FutureExt, StreamExt};
pub use task_local_context::current;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// A task-local context for propagating metadata, deadlines, and other request-scoped values.
pub trait Context: Send + Sync + 'static {
    /// Get the deadline for the current context.
    fn deadline(&self) -> Option<Instant>;

    /// Create a new context with the given deadline.
    fn with_deadline(&self, deadline: Instant) -> Arc<dyn Context>;

    /// Get a value from the context extensions.
    fn get(&self, type_id: TypeId) -> Option<&(dyn Any + Send + Sync)>;

    /// Create a new context with the given value.
    fn with_value(&self, type_id: TypeId, value: Arc<dyn Any + Send + Sync>) -> Arc<dyn Context>;
}

#[derive(Clone, Default)]
struct ContextInner {
    deadline: Option<Instant>,
    extensions: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

#[derive(Clone, Default)]
pub(crate) struct ContextImpl {
    inner: Arc<ContextInner>,
}

impl Context for ContextImpl {
    fn deadline(&self) -> Option<Instant> {
        self.inner.deadline
    }

    fn with_deadline(&self, deadline: Instant) -> Arc<dyn Context> {
        let mut inner = (*self.inner).clone();
        inner.deadline = Some(deadline);
        Arc::new(Self {
            inner: Arc::new(inner),
        })
    }

    fn get(&self, type_id: TypeId) -> Option<&(dyn Any + Send + Sync)> {
        self.inner.extensions.get(&type_id).map(|v| &**v as _)
    }

    fn with_value(&self, type_id: TypeId, value: Arc<dyn Any + Send + Sync>) -> Arc<dyn Context> {
        let mut inner = (*self.inner).clone();
        inner.extensions.insert(type_id, value);
        Arc::new(Self {
            inner: Arc::new(inner),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_context_has_no_deadline_or_extensions() {
        let context = ContextImpl::default();
        assert!(context.deadline().is_none());
        assert!(context.get(TypeId::of::<i32>()).is_none());
    }

    #[test]
    fn with_deadline_sets_deadline_and_preserves_original() {
        let context = ContextImpl::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let context_with_deadline = context.with_deadline(deadline);

        assert_eq!(context_with_deadline.deadline(), Some(deadline));
        // Original context should remain unchanged
        assert!(context.deadline().is_none());
    }

    #[test]
    fn with_value_stores_extension_and_preserves_original() {
        let context = ContextImpl::default();

        #[derive(Debug, PartialEq)]
        struct MyValue(i32);

        let context_with_value = context.with_value(TypeId::of::<MyValue>(), Arc::new(MyValue(42)));

        let value = context_with_value
            .get(TypeId::of::<MyValue>())
            .and_then(|v| v.downcast_ref::<MyValue>());
        assert_eq!(value, Some(&MyValue(42)));

        // Original context should not have the value
        assert!(context.get(TypeId::of::<MyValue>()).is_none());
    }

    #[test]
    fn with_value_overwrites_existing_extension_and_preserves_previous() {
        let context = ContextImpl::default();

        #[derive(Debug, PartialEq)]
        struct MyValue(i32);

        let ctx1 = context.with_value(TypeId::of::<MyValue>(), Arc::new(MyValue(10)));
        let ctx2 = ctx1.with_value(TypeId::of::<MyValue>(), Arc::new(MyValue(20)));

        let val1 = ctx1
            .get(TypeId::of::<MyValue>())
            .and_then(|v| v.downcast_ref::<MyValue>());
        let val2 = ctx2
            .get(TypeId::of::<MyValue>())
            .and_then(|v| v.downcast_ref::<MyValue>());

        assert_eq!(val1, Some(&MyValue(10)));
        assert_eq!(val2, Some(&MyValue(20)));
    }
}
