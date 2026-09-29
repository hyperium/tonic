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
//!    The format matches [gRFC A27] — a JSON object with `xds_servers`,
//!    each entry carrying a `server_uri` and the `channel_creds` types the
//!    client may offer, plus an optional `node`.
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
//! | Programmatic (builder) | [`BootstrapConfig::builder`] then [`XdsChannelConfig::with_bootstrap`] |
//! | Programmatic (JSON) | [`BootstrapConfig::from_json`] then [`XdsChannelConfig::with_bootstrap`] |
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
//!   "xds_servers": [{
//!     "server_uri": "xds.example.com:443",
//!     "channel_creds": [{"type": "tls"}]
//!   }],
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
//!     "xds_servers": [{
//!         "server_uri": "xds.example.com:443",
//!         "channel_creds": [{"type": "tls"}]
//!     }],
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
//! ### Using the builder
//!
//! ```rust,no_run
//! use tonic_xds::{BootstrapConfig, ChannelCredentialType, XdsChannelBuilder, XdsChannelConfig, XdsUri};
//!
//! let bootstrap = BootstrapConfig::builder("xds.example.com:443")
//!     .channel_creds([ChannelCredentialType::Tls])
//!     .node_id("my-node")
//!     .node_cluster("my-cluster")
//!     .build()
//!     .unwrap();
//!
//! let target = XdsUri::parse("xds:///myservice:50051").unwrap();
//! let channel = XdsChannelBuilder::new(
//!     XdsChannelConfig::new(target).with_bootstrap(bootstrap),
//! ).build_grpc_channel().unwrap();
//!
//! // let client = MyServiceClient::new(channel);
//! ```
//!
//! ## ADS TLS Security (gRFC A65)
//!
//! The `tls` channel credential secures the control-plane ADS connection. Its
//! optional config accepts CA roots, an mTLS identity, and a refresh interval:
//!
//! ```json
//! {
//!   "xds_servers": [{
//!     "server_uri": "xds.example.com:443",
//!     "channel_creds": [{
//!       "type": "tls",
//!       "config": {
//!         "ca_certificate_file": "/etc/certs/ca.pem",
//!         "certificate_file": "/etc/certs/client.pem",
//!         "private_key_file": "/etc/certs/client.key",
//!         "refresh_interval": "60s"
//!       }
//!     }]
//!   }]
//! }
//! ```
//!
//! If `ca_certificate_file` is omitted, system roots are used. The certificate
//! and private-key fields must be set together; omitting both configures normal
//! server-authenticated TLS. Files are refreshed in the background (every 600s
//! by default), and each ADS reconnect uses the latest successfully read
//! snapshot.
//!
//! `google_default` is parsed for bootstrap forward compatibility but is not
//! currently implemented. Place `tls` or `insecure` after it as a supported
//! fallback.
//!
//! ## Data-plane TLS Security (gRFC A29)
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
//!   "xds_servers": [{
//!     "server_uri": "xds.example.com:443",
//!     "channel_creds": [{"type": "tls"}]
//!   }],
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
//! | ADS TLS and mTLS credentials | [A65] | Supported |
//! | TLS endpoint connections | [A29] | Supported |
//! | Least-request load balancing | [A48] | Planned |
//!
//! [A27]: https://github.com/grpc/proposal/blob/master/A27-xds-global-load-balancing.md
//! [A28]: https://github.com/grpc/proposal/blob/master/A28-xds-traffic-splitting-and-routing.md
//! [A29]: https://github.com/grpc/proposal/blob/master/A29-xds-tls-security.md
//! [A48]: https://github.com/grpc/proposal/blob/master/A48-xds-least-request-lb-policy.md
//! [A63]: https://github.com/grpc/proposal/blob/master/A63-xds-string-matcher-ignore-case.md
//! [A65]: https://github.com/grpc/proposal/blob/master/A65-xds-mtls-creds-in-bootstrap.md

pub(crate) mod client;
pub(crate) mod common;
pub(crate) mod xds;

pub use client::channel::{
    BuildError, XdsChannel, XdsChannelBuilder, XdsChannelConfig, XdsChannelGrpc,
    XdsTransportChannel,
};
pub use client::endpoint::{
    ClusterConfig, Connector, EndpointAddress, EndpointChannel, MakeConnector,
};
pub use client::retry::{
    GrpcRetryClassifierFactory, RetryClassifier, RetryClassifierFactory, RetryOutcome,
    is_retryable_connection_error,
};
pub use client::route::PreRouteInterceptor;
pub use common::async_util::BoxFuture;
pub use shared_http_body::SharedBody;
pub use xds::bootstrap::{
    BootstrapConfig, BootstrapConfigBuilder, BootstrapError, ChannelCredentialType,
};
pub use xds::cert_provider_config::TlsChannelCredentials;
pub use xds::resource::route_config::{RouteConfigMetadata, TypedMetadata};
pub use xds::uri::{XdsUri, XdsUriError};
/// Re-export of the error type returned by
/// [`ClusterSecurityConfig::from_cluster_bytes`], so callers can name it
/// without a direct `xds-client` dependency.
pub use xds_client::Error as XdsError;
pub use xds_client::TonicCallCredentials;

#[cfg(feature = "_tls-any")]
pub use client::endpoint::{ClusterTlsConfig, ClusterTlsError};
/// Re-export of the rustls trait returned by [`ClusterTlsConfig::build_verifier`],
/// so custom transports can name it without a direct `rustls` dependency.
#[cfg(feature = "_tls-any")]
pub use rustls::client::danger::ServerCertVerifier;
#[cfg(feature = "_tls-any")]
pub use xds::cert_provider::{CertProviderError, CertificateData, CertificateProvider, Identity};
pub use xds::resource::security::ClusterSecurityConfig;

pub use xds_client::{Instrument, InstrumentKind, KeyValue, MetricsRecorder, StringValue, Value};

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
