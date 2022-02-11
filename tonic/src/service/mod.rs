//! Utilities for using Tower services with Tonic.

pub mod interceptor;

#[doc(inline)]
#[allow(deprecated)]
pub use self::interceptor::{
    async_interceptor, interceptor, interceptor_fn, AsyncInterceptor, Interceptor,
};
