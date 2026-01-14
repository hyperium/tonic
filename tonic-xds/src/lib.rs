//! # tonic-xds
//!
//! xDS (discovery service) support for [Tonic](https://docs.rs/tonic) gRPC clients as well as
//! general [Tower](https://docs.rs/tower) services.
//!
//! This crate provides an xDS-enabled Tonic Channel that automatically discovers,
//! routesand load balances across endpoints using the xDS protocol. The xDS features will align with
//! the [gRPC xDS features](https://github.com/grpc/grpc/blob/master/doc/grpc_xds_features.md)
//!
//! ## Current Planned Features:
//!
//! - LDS / RDS / CDS / EDS subscriptions via ADS stream.
//! - Client-side P2C load balancing
//!
//! ## Example
//!
//! ```rust,no_run
//! use tonic_xds::{XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc, XdsUri};
//! 
//! let xds_uri = XdsUri::parse(
//!   "xds:///xds-management-server-local-test:50051"
//! ).expect("fail to parse valid xDS URI");
//! 
//! let xds_channel = XdsChannelBuilder::with_config(
//!   XdsChannelConfig::default().with_uri(xds_uri)
//! ).build_grpc_channel();
//!
//! // Use with your generated gRPC client
//! // let client = MyServiceClient::new(xds_channel);
//! // client.my_rpc_method(...).await;
//! ```
//!
//! ## How it works
//!
//! `XdsChannel` connects to an xDS management server and subscribes to resource updates for
//! listeners, routes, clusters, and endpoints. Requests are automatically routed and load balanced
//! in stacked Tower services based on the xDS configuration.

pub(crate) mod client;
pub(crate) mod xds;

pub use client::channel::{XdsChannel, XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc};
pub use xds::uri::{XdsUri, XdsUriError};

#[cfg(test)]
pub(crate) mod testutil;