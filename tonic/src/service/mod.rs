//! Utilities for using Tower services with Tonic.

pub mod interceptor;

#[doc(inline)]
#[allow(deprecated)]
pub use self::interceptor::{interceptor, interceptor_fn, Interceptor};
