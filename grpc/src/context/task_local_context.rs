//! Task local context management.
//!
//! # Implementation Details
//!
//! This module implements a task-local context storage mechanism that is runtime agnostic.
//! It works by using a `std::thread_local` to store the context and swapping it in and out
//! of scope when the future is polled. This allows the context to be available to any
//! code running within the scope of the future, even if it is deeply nested.
//!
//! The implementation is very similar to `tokio::task_local` in terms of performance and
//! mechanics, but it does not depend on the Tokio runtime.
//!
//! # Performance
//!
//! It is important to note that this is **not** a zero-cost abstraction. Every time the
//! future is polled (i.e., every suspend/resume point), a cheap `Arc` clone is performed
//! to ensure the context is correctly set and restored. This overhead is generally minimal
//! but should be considered in performance-critical paths.

use super::Context;
use super::ContextImpl;
use pin_project_lite::pin_project;
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use tokio_stream::Stream;

thread_local! {
    static CURRENT: RefCell<Option<Arc<dyn Context>>> = const { RefCell::new(None) };
}

/// Get the current context.
///
/// This function returns the context associated with the current task.
/// If no context is set, it returns a default context.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use grpc::context::{self, Context, FutureExt};
///
/// #[tokio::main]
/// async fn main() {
///     // By default, an empty context is returned
///     let ctx = context::current();
///     assert!(ctx.deadline().is_none());
///
///     // You can set the context for a future
///     let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
///     let ctx = ctx.with_deadline(deadline);
///
///     let future = async {
///         let current_ctx = context::current();
///         assert_eq!(current_ctx.deadline(), Some(deadline));
///     };
///
///     future.with_context(ctx).await;
/// }
/// ```
pub fn current() -> Arc<dyn Context> {
    CURRENT.with(|ctx| {
        ctx.borrow()
            .as_ref()
            .map(|c| c.clone())
            .unwrap_or_else(|| Arc::new(ContextImpl::default()))
    })
}

pin_project! {
    pub struct ContextScope<T> {
        #[pin]
        inner: T,
        context: Arc<dyn Context>,
    }
}

impl<T> ContextScope<T> {
    pub fn new(inner: T, context: Arc<dyn Context>) -> Self {
        Self { inner, context }
    }
}

struct ContextGuard {
    previous: Option<Arc<dyn Context>>,
}

impl ContextGuard {
    fn new(context: Arc<dyn Context>) -> Self {
        let previous = CURRENT.with(|ctx| ctx.borrow_mut().replace(context));
        Self { previous }
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT.with(|ctx| {
            if let Some(prev) = self.previous.take() {
                ctx.borrow_mut().replace(prev);
            } else {
                ctx.borrow_mut().take();
            }
        });
    }
}

impl<F: Future> Future for ContextScope<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _guard = ContextGuard::new(this.context.clone());
        this.inner.poll(cx)
    }
}

impl<S: Stream> Stream for ContextScope<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let _guard = ContextGuard::new(this.context.clone());
        this.inner.poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[test]
    fn test_no_context_set_current_returns_default() {
        let ctx = current();
        assert!(ctx.deadline().is_none());
    }

    #[tokio::test]
    async fn test_future_wrapped_in_context_scope_sees_context() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let future = async {
            let current_ctx = current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
        };

        ContextScope::new(future, ctx).await;

        // After scope, context should be reset (or default)
        assert!(current().deadline().is_none());
    }

    #[tokio::test]
    async fn test_stream_wrapped_in_context_scope_sees_context() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let stream = async_stream::stream! {
            let current_ctx = current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
            yield 1;
        };

        let scoped_stream = ContextScope::new(stream, ctx);
        tokio::pin!(scoped_stream);
        scoped_stream.next().await;

        assert!(current().deadline().is_none());
    }

    #[tokio::test]
    async fn test_nested_context_scopes_restore_previous_context() {
        let ctx1 = ContextImpl::default();
        let deadline1 = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx1 = ctx1.with_deadline(deadline1);

        let ctx2 = ContextImpl::default();
        let deadline2 = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let ctx2 = ctx2.with_deadline(deadline2);

        let future = async move {
            assert_eq!(current().deadline(), Some(deadline1));

            let inner_future = async {
                assert_eq!(current().deadline(), Some(deadline2));
            };

            ContextScope::new(inner_future, ctx2).await;

            assert_eq!(current().deadline(), Some(deadline1));
        };

        ContextScope::new(future, ctx1).await;
        assert!(current().deadline().is_none());
    }

    #[tokio::test]
    async fn test_spawned_task_with_context_scope_sees_context() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let future = async move {
            // This code runs in a spawned task
            let current_ctx = current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
        };

        // Spawn a new task, but wrap the future with context
        let handle = tokio::spawn(ContextScope::new(future, ctx));
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_spawned_task_without_context_scope_does_not_inherit_context() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        // Set the context for the current task
        let future = async {
            // Spawn a new task WITHOUT wrapping it in ContextScope
            let handle = tokio::spawn(async {
                let current_ctx = current();
                // Should NOT have the deadline
                assert!(current_ctx.deadline().is_none());
            });
            handle.await.unwrap();
        };

        ContextScope::new(future, ctx).await;
    }

    #[tokio::test]
    async fn test_context_propagates_to_nested_futures() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let inner_future = async {
            let current_ctx = current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
        };

        let outer_future = async {
            inner_future.await;
        };

        ContextScope::new(outer_future, ctx).await;
    }
}
