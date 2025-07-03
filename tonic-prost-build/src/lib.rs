//! Prost build integration for tonic.
//!
//! This crate provides code generation for gRPC services using protobuf definitions
//! via the [`prost`] ecosystem.
//!
//! # Example
//!
//! ```rust,ignore
//! use tonic_prost_build::configure;
//!
//! fn main() {
//!     configure()
//!         .compile_protos(&["proto/service.proto"], &["proto"])
//!         .unwrap();
//! }
//! ```

#![warn(
    missing_docs,
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![doc(html_root_url = "https://docs.rs/tonic-prost-build/0.14.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

//! Prost build integration for tonic.
//!
//! This crate provides the build-time code generation functionality for tonic
//! when using prost as the protobuf implementation.
//!
//! # Example
//!
//! ```rust,ignore
//! use tonic_prost_build::configure;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     configure()
//!         .build_server(true)
//!         .build_client(true)
//!         .compile_protos(&["proto/service.proto"], &["proto"])?;
//!     Ok(())
//! }
//! ```

#![warn(
    missing_docs,
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![doc(html_root_url = "https://docs.rs/tonic-prost-build/0.14.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

mod prost;

// Re-export core build functionality from tonic-build
pub use tonic_build::{Attributes, Method, Service};

// Re-export prost-specific functionality
pub use crate::prost::{compile_fds, compile_protos, configure, Builder};

// Re-export prost types that users might need
pub use prost_build::Config;
pub use prost_types::FileDescriptorSet;
