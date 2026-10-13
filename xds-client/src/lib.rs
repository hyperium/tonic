/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

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
//! use xds_client::{XdsClient, ClientConfig, Node, ResourceEvent};
//!
//! // Create node and configuration
//! let node = Node::new("grpc", "1.0").with_id("my-node");
//! let config = ClientConfig::new(node, "https://xds.example.com:443");
//!
//! // Build client with transport, codec, and runtime
//! let client = XdsClient::builder(config, transport, codec, runtime).build();
//!
//! // Watch for Listener resources
//! let mut watcher = client.watch::<Listener>("my-listener").await;
//! while let Some(event) = watcher.next().await {
//!     match event {
//!         ResourceEvent::ResourceChanged { result: Ok(resource), done } => {
//!             // Process the resource, possibly add cascading watches.
//!             client.watch::<RouteConfiguration>(&resource.route_name()).await;
//!             // Signal is sent automatically when done is dropped
//!         }
//!         ResourceEvent::ResourceChanged { result: Err(error), .. } => {
//!             // Resource was invalidated (validation error or deleted)
//!             eprintln!("Resource invalidated: {}", error);
//!         }
//!         ResourceEvent::AmbientError { error, .. } => {
//!             // Non-fatal error, continue using cached resource
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
pub mod metrics;
pub mod resource;
pub mod runtime;
pub mod transport;

pub use client::config::{ClientConfig, ServerConfig};
pub use client::retry::{Backoff, RetryPolicy};
pub use client::watch::{ProcessingDone, ResourceEvent, ResourceWatcher};
pub use client::{XdsClient, XdsClientBuilder};
pub use codec::XdsCodec;
pub use error::{Error, Result};
pub use message::{DiscoveryRequest, DiscoveryResponse, ErrorDetail, Locality, Node, ResourceAny};
pub use metrics::{Instrument, InstrumentKind, KeyValue, MetricsRecorder, StringValue, Value};

pub use resource::{DecodeResult, DecodedResource, Resource};
pub use runtime::Runtime;
pub use transport::{Transport, TransportBuilder, TransportReceiver, TransportSender};

// Tokio runtime
#[cfg(feature = "rt-tokio")]
pub use runtime::tokio::TokioRuntime;

// Tonic transport
#[cfg(feature = "transport-tonic")]
pub use transport::tonic::{TonicCallCredentials, TonicTransport, TonicTransportBuilder};

// Prost codec
#[cfg(feature = "codegen-prost")]
pub use codec::prost::ProstCodec;
