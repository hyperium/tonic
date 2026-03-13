//! Utilities for async operations.

use std::future::Future;
use std::pin::Pin;

pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
