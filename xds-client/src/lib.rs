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
//! use xds_client::{XdsClient, ClientConfig, ResourceEvent};
//!
//! // Create configuration with node identification
//! let config = ClientConfig::with_node_id("my-node");
//!
//! // Build client with transport, codec, and runtime
//! let client = XdsClient::builder(config, transport, codec, runtime).build();
//!
//! // Watch for Listener resources
//! let mut watcher = client.watch::<Listener>("my-listener");
//! while let Some(event) = watcher.next().await {
//!     match event {
//!         ResourceEvent::ResourceChanged { resource, done } => {
//!             // Process the resource, possibly add cascading watches.
//!             client.watch::<RouteConfiguration>(&resource.route_name());
//!             done.complete();
//!         }
//!         ResourceEvent::ResourceError { error, done } => {
//!             eprintln!("Error: {}", error);
//!             done.complete();
//!         }
//!         ResourceEvent::AmbientError { error, .. } => {
//!             // Can also rely on auto-signal on drop
//!             eprintln!("Ambient error: {}", error);
//!         }
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
pub mod codec;
pub mod error;
pub mod message;
pub mod resource;
pub mod runtime;
pub mod transport;

pub use client::config::ClientConfig;
pub use client::watch::{ProcessingDone, ResourceEvent, ResourceWatcher};
pub use client::{XdsClient, XdsClientBuilder};
pub use codec::XdsCodec;
pub use error::{Error, Result};
pub use message::{DiscoveryRequest, DiscoveryResponse, ErrorDetail, Locality, Node, ResourceAny};
pub use resource::{DecodedResource, Resource};
pub use runtime::Runtime;
pub use transport::{Transport, TransportStream};

// Tokio runtime
#[cfg(feature = "rt-tokio")]
pub use runtime::tokio::TokioRuntime;

// Tonic transport
#[cfg(feature = "transport-tonic")]
pub use transport::tonic::TonicTransport;

// Prost codec
#[cfg(feature = "codegen-prost")]
pub use codec::prost::ProstCodec;
