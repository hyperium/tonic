//! Utilities for using Tower services with Tonic.

pub mod interceptor;

#[doc(inline)]
pub use self::interceptor::{interceptor, Interceptor};
