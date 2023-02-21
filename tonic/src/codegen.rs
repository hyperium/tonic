//! Codegen exports used by `tonic-build`.

pub use async_trait::async_trait;
pub use futures_core;
pub use futures_util::future::{ok, poll_fn, Ready};

pub use std::future::Future;
pub use std::pin::Pin;
pub use std::sync::Arc;
pub use std::task::{Context, Poll};
pub use tower_service::Service;
pub type StdError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub use crate::codec::{CompressionEncoding, EnabledCompressionEncodings};
pub use crate::extensions::GrpcMethod;
pub use crate::service::interceptor::InterceptedService;
pub use bytes::Bytes;
pub use http;
pub use http_body::Body;

pub type BoxFuture<T, E> = self::Pin<Box<dyn self::Future<Output = Result<T, E>> + Send + 'static>>;
pub type BoxStream<T> =
    self::Pin<Box<dyn futures_core::Stream<Item = Result<T, crate::Status>> + Send + 'static>>;

pub use crate::body::empty_body;
