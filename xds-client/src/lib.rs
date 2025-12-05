//! A Rust implementation of [xDS](https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol) client.
//!
//! # Feature Flags
//!
//! - `transport-tonic`: Enables the use of the `tonic` transport. This enables `rt-tokio` and `codegen-prost` features. Enabled by default.
//! - `rt-tokio`: Enables the use of the `tokio` runtime. Enabled by default.
//! - `codegen-prost`: Enables the use of the `prost` codec generated resources. Enabled by default.

pub mod client;
pub mod error;
pub mod resource;
pub mod runtime;
pub mod transport;
