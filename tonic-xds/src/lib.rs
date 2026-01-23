//! # tonic-xds
//!
//! xDS (discovery service) support for [Tonic](https://docs.rs/tonic) gRPC clients as well as
//! general [`tower::Service`].
//!
//! This crate provides an xDS-enabled [`tonic::client::GrpcService`] implementation ([`XdsChannelGrpc`])
//! that automatically discovers, routes and load-balances across endpoints using the xDS protocol.
//! The implementation will align with the
//! [gRPC xDS features](https://github.com/grpc/grpc/blob/master/doc/grpc_xds_features.md).
//!
//! In addition to gRPC, this crate also provides a generic [`tower::Service`] implementation ([`XdsChannel`])
//! for enabling xDS features for generic Http clients. This can be used to support both gRPC and Http
//! clients by the same xDS management server.
//!
//! ## Current Planned Features:
//!
//! - LDS / RDS / CDS / EDS subscriptions via ADS stream.
//! - Client-side P2C load balancing
//! - Other features will be added in future releases.
//!
//! ## Example
//!
//! ```rust,no_run
//! use tonic_xds::{XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc, XdsUri};
//!
//! let target_uri = XdsUri::parse(
//!   "xds:///myservice:50051"
//! ).expect("fail to parse valid target URI");
//!
//! let xds_channel = XdsChannelBuilder::with_config(
//!   XdsChannelConfig::default().with_target_uri(target_uri)
//! ).build_grpc_channel();
//!
//! // Use with your generated gRPC client
//! // let client = MyServiceClient::new(xds_channel);
//! // client.my_rpc_method(...).await;
//! ```
//!
//! ## How it works
//!
//! [`XdsChannelGrpc`] connects to an xDS management server and subscribes to resource updates for
//! listeners, routes, clusters, and endpoints. Requests are automatically routed and load-balanced
//! in stacked [`tower::Service`]s that implement the [gRPC xDS features](https://github.com/grpc/grpc/blob/master/doc/grpc_xds_features.md).

pub(crate) mod client;
pub(crate) mod common;
pub(crate) mod xds;

pub use client::channel::{XdsChannel, XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc};
pub use xds::uri::{XdsUri, XdsUriError};

#[cfg(test)]
pub(crate) mod testutil;
