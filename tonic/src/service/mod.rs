//! Utilities for using Tower services with Tonic.

pub mod interceptor;
#[cfg(feature = "router")]
mod router;

#[doc(inline)]
pub use self::interceptor::{interceptor, Interceptor};
#[doc(inline)]
#[cfg(feature = "router")]
pub use self::router::{Routes, RoutesBuilder};
