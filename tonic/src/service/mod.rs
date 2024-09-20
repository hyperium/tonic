//! Utilities for using Tower services with Tonic.

pub mod interceptor;
#[cfg(feature = "router")]
pub(crate) mod router;

#[doc(inline)]
pub use self::interceptor::{interceptor, Interceptor};
#[doc(inline)]
#[cfg(feature = "router")]
pub use self::router::{Routes, RoutesBuilder};
#[cfg(feature = "router")]
pub use axum::{body::Body as AxumBody, Router as AxumRouter};
