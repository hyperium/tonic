//! A Rust implementation of [xDS](https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol) client.
//!
//! This crate provides a protocol-agnostic xDS client. It handles:
//! - ADS stream management (connection, reconnection, etc.)
//! - Resource subscription and watching
//! - Version/nonce tracking and ACK/NACK
//!
//! It does NOT contain gRPC-specific logic such as:
//! - LDS -> RDS -> CDS -> EDS cascading
//! - gRPC-specific resource validation
//! - Service config generation
//!
//! Instead a gRPC library can use this crate to build these features.
//!
//! # Example
//!
//! ```ignore
//! use xds_client::{XdsClient, ClientConfig, Resource};
//!
//! let config = ClientConfig::new("http://localhost:10000", "my-node");
//! let client = XdsClient::builder(config)
//!     .build(transport, runtime)
//!     .await?;
//!
//! let mut watcher = client.watch::<Listener>("my-listener");
//! while let Some(event) = watcher.next().await {
//!     match event {
//!         ResourceEvent::Upsert(resource) => { /* handle resource update */ }
//!         // ... handle other events ...
//!     }
//! }
//! ```
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

pub use client::config::ClientConfig;
pub use client::watch::{ResourceEvent, ResourceWatcher};
pub use client::{XdsClient, XdsClientBuilder};
pub use error::{Error, Result};
pub use resource::Resource;
pub use runtime::Runtime;
pub use transport::{DiscoveryRequest, DiscoveryResponse, Transport, TransportStream};
