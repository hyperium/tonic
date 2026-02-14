//! Extension traits for `Future` and `Stream` to provide context propagation.
//!
//! This module provides the [`FutureExt`] and [`StreamExt`] traits, which allow
//! attaching a [`Context`] to a [`Future`] or [`Stream`]. This ensures that the
//! context is set as the current task-local context whenever the future or stream
//! is polled.

use std::future::Future;
use std::sync::Arc;
use tokio_stream::Stream;

use super::task_local_context;
use super::Context;

/// Extension trait for `Future` to provide context propagation.
///
/// This trait allows attaching a [`Context`] to a [`Future`], ensuring that the context
/// is set as the current task-local context whenever the future is polled.
///
/// # Examples
///
/// ```rust
/// # use std::sync::Arc;
/// # use grpc::context::{Context, FutureExt};
/// # async fn example() {
/// let context = grpc::context::current();
/// let future = async {
///     // Context is available here
///     assert!(grpc::context::current().deadline().is_none());
/// };
///
/// future.with_context(context).await;
/// # }
/// ```
pub trait FutureExt: Future {
    /// Attach a context to this future.
    ///
    /// The context will be set as the current task-local context whenever the future is polled.
    fn with_context(self, context: Arc<dyn Context>) -> impl Future<Output = Self::Output>
    where
        Self: Sized,
    {
        task_local_context::ContextScope::new(self, context)
    }
}

impl<F: Future> FutureExt for F {}

/// Extension trait for `Stream` to provide context propagation.
///
/// This trait allows attaching a [`Context`] to a [`Stream`], ensuring that the context
/// is set as the current task-local context whenever the stream is polled.
///
/// # Examples
///
/// ```rust
/// # use std::sync::Arc;
/// # use grpc::context::{Context, StreamExt};
/// # use tokio_stream::StreamExt as _;
/// # async fn example() {
/// let context = grpc::context::current();
/// let stream = tokio_stream::iter(vec![1, 2, 3]);
///
/// let mut scoped_stream = stream.with_context(context);
///
/// while let Some(item) = scoped_stream.next().await {
///     // Context is available here
///     assert!(grpc::context::current().deadline().is_none());
/// }
/// # }
/// ```
pub trait StreamExt: Stream {
    /// Attach a context to this stream.
    ///
    /// The context will be set as the current task-local context whenever the stream is polled.
    fn with_context(self, context: Arc<dyn Context>) -> impl Stream<Item = Self::Item>
    where
        Self: Sized,
    {
        task_local_context::ContextScope::new(self, context)
    }
}

impl<S: Stream> StreamExt for S {}

#[cfg(test)]
mod tests {
    use super::super::ContextImpl;
    use super::*;
    use tokio_stream::StreamExt as _;

    #[tokio::test]
    async fn test_future_ext_attaches_context_correctly() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let future = async {
            let current_ctx = super::task_local_context::current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
        };

        future.with_context(ctx).await;
    }

    #[tokio::test]
    async fn test_stream_ext_attaches_context_correctly() {
        let ctx = ContextImpl::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ctx = ctx.with_deadline(deadline);

        let stream = async_stream::stream! {
            let current_ctx = super::task_local_context::current();
            assert_eq!(current_ctx.deadline(), Some(deadline));
            yield 1;
        };

        let scoped_stream = stream.with_context(ctx);
        tokio::pin!(scoped_stream);
        scoped_stream.next().await;
    }
}
