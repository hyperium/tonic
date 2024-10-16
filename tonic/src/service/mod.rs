//! Utilities for using Tower services with Tonic.

pub mod interceptor;
pub(crate) mod layered;
#[cfg(feature = "router")]
pub(crate) mod router;

#[doc(inline)]
#[allow(deprecated)]
pub use self::interceptor::interceptor;
#[doc(inline)]
pub use self::interceptor::{Interceptor, InterceptorLayer};
pub use self::layered::{LayerExt, Layered};
#[doc(inline)]
#[cfg(feature = "router")]
pub use self::router::{Routes, RoutesBuilder};
#[cfg(feature = "router")]
pub use axum::{body::Body as AxumBody, Router as AxumRouter};
