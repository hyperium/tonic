//! # tonic-xds
//!
//! xDS-based service discovery, routing, and load balancing for
//! [Tonic](https://docs.rs/tonic) gRPC clients.
//!
//! This crate provides [`XdsChannelGrpc`], a [`tonic::client::GrpcService`]
//! that connects to an xDS management server (via ADS) and automatically
//! discovers, routes, and load-balances requests across endpoints. The
//! implementation follows the [gRPC xDS features] specification.
//!
//! [gRPC xDS features]: https://github.com/grpc/grpc/blob/master/doc/grpc_xds_features.md
//!
//! ## Getting started
//!
//! 1. **Provide a bootstrap configuration** that tells the client where
//!    the xDS management server lives and what node identity to present.
//!    The format matches [gRFC A27] — a JSON object with `xds_servers`
//!    and an optional `node`.
//!
//! 2. **Build the channel** with [`XdsChannelBuilder`], pointing it at
//!    an `xds:///` target URI.
//!
//! 3. **Pass the channel** to your generated gRPC client.
//!
//! [gRFC A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
//!
//! ## Bootstrap configuration
//!
//! The bootstrap can be supplied in three ways (in order of precedence):
//!
//! | Method | How |
//! |--------|-----|
//! | Programmatic | [`BootstrapConfig::from_json`] then [`XdsChannelConfig::with_bootstrap`] |
//! | Environment (explicit) | [`XdsChannelConfig::with_bootstrap_from_env`] |
//! | Environment (implicit) | Omit bootstrap; the builder loads from env vars automatically |
//!
//! The environment variables checked are:
//! - `GRPC_XDS_BOOTSTRAP` — path to a JSON file
//! - `GRPC_XDS_BOOTSTRAP_CONFIG` — inline JSON string
//!
//! Minimal bootstrap JSON:
//!
//! ```json
//! {
//!   "xds_servers": [{"server_uri": "xds.example.com:443"}],
//!   "node": {"id": "my-node"}
//! }
//! ```
//!
//! ## Examples
//!
//! ### Using environment variables (simplest)
//!
//! ```rust,no_run
//! // Requires GRPC_XDS_BOOTSTRAP or GRPC_XDS_BOOTSTRAP_CONFIG to be set.
//! use tonic_xds::{XdsChannelBuilder, XdsChannelConfig, XdsUri};
//!
//! let target = XdsUri::parse("xds:///myservice:50051").unwrap();
//! let channel = XdsChannelBuilder::new(XdsChannelConfig::new(target))
//!     .build_grpc_channel()
//!     .unwrap();
//!
//! // let client = MyServiceClient::new(channel);
//! ```
//!
//! ### Using inline JSON
//!
//! ```rust,no_run
//! use tonic_xds::{BootstrapConfig, XdsChannelBuilder, XdsChannelConfig, XdsUri};
//!
//! let bootstrap = BootstrapConfig::from_json(r#"{
//!     "xds_servers": [{"server_uri": "xds.example.com:443"}],
//!     "node": {"id": "my-node", "cluster": "my-cluster"}
//! }"#).unwrap();
//!
//! let target = XdsUri::parse("xds:///myservice:50051").unwrap();
//! let channel = XdsChannelBuilder::new(
//!     XdsChannelConfig::new(target).with_bootstrap(bootstrap),
//! ).build_grpc_channel().unwrap();
//!
//! // let client = MyServiceClient::new(channel);
//! ```
//!
//! ## TLS Security (gRFC A29)
//!
//! Upstream data-plane TLS is enabled when:
//!
//! 1. The crate is built with `tls-ring` *or* `tls-aws-lc` (exactly one).
//! 2. The bootstrap JSON declares `certificate_providers` entries — each
//!    referenced by `instance_name` in CDS resources.
//! 3. A CDS `Cluster` carries `transport_socket: UpstreamTlsContext` naming
//!    those instances (configured on the xDS control plane).
//!
//! Only the `file_watcher` plugin is built in. It reads PEM files from disk
//! and refreshes them on `refresh_interval` (default 600s) — rotated certs
//! reach existing TLS connectors on the next handshake.
//!
//! ```json
//! {
//!   "xds_servers": [{"server_uri": "xds.example.com:443"}],
//!   "certificate_providers": {
//!     "root_ca":  { "plugin_name": "file_watcher", "config": {
//!       "ca_certificate_file": "/etc/certs/ca.pem"
//!     }},
//!     "identity": { "plugin_name": "file_watcher", "config": {
//!       "certificate_file":  "/etc/certs/cert.pem",
//!       "private_key_file":  "/etc/certs/key.pem",
//!       "refresh_interval":  "60s"
//!     }}
//!   }
//! }
//! ```
//!
//! When `match_typed_subject_alt_names` is set on the cluster's validation
//! context, the server cert's SAN list must match one of the configured
//! matchers ("any" semantics). An empty matcher list accepts any cert
//! chained to the configured CA roots.
//!
//! CDS updates that change a cluster's `transport_socket` rebuild that
//! cluster's connector — new endpoint connections pick up the new config;
//! existing TLS sessions continue.
//!
//! ## xDS features
//!
//! | Feature | gRFC | Status |
//! |---------|------|--------|
//! | Bootstrap configuration | [A27] | Supported |
//! | xDS transport (ADS, SotW) | [A27] | Supported |
//! | LDS / RDS / CDS / EDS resource cascade | [A27] | Supported |
//! | Route matching (domain, path, headers) | [A28] | Supported |
//! | Weighted cluster traffic splitting | [A28] | Supported |
//! | Case-insensitive header matching | [A63] | Supported |
//! | Client-side P2C load balancing | | Supported |
//! | TLS endpoint connections | [A29] | Supported |
//! | Least-request load balancing | [A48] | Planned |
//!
//! [A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
//! [A28]: https://github.com/grpc/proposal/blob/master/A28-xds-traffic-splitting-and-routing.md
//! [A29]: https://github.com/grpc/proposal/blob/master/A29-xds-tls-security.md
//! [A48]: https://github.com/grpc/proposal/blob/master/A48-xds-least-request-lb-policy.md
//! [A63]: https://github.com/grpc/proposal/blob/master/A63-xds-string-matcher-ignore-case.md

pub(crate) mod client;
pub(crate) mod common;
pub(crate) mod xds;

pub use client::channel::{
    BuildError, XdsChannel, XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc,
};
pub use xds::bootstrap::{BootstrapConfig, BootstrapError};
pub use xds::uri::{XdsUri, XdsUriError};

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
