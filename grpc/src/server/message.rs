//! Traits for accessing view and mutable view types of messages.
//!
//! These traits are needed to support Protobuf's design decision to prefer view and mut proxy types
//! over references. See <https://protobuf.dev/reference/rust/rust-design-decisions/#view-mut-proxy-types>
//! for more details.
//!
//! These traits allow for support of view and mut types while defaulting to regular references for
//! non-Protobuf classes that need gRPC support.

pub mod as_mut;
pub mod as_view;

pub use as_mut::AsMut;
pub use as_view::AsView;

#[cfg(feature = "protobuf")]
pub mod protobuf;

#[cfg(all(feature = "prost", not(feature = "protobuf")))]
pub mod prost;
