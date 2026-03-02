//! xDS resource type implementations.
//!
//! Each module implements [`xds_client::Resource`] for one of the four resource types:
//! - [`ListenerResource`] (LDS)
//! - [`RouteConfigResource`] (RDS)
//! - [`ClusterResource`] (CDS)
//! - [`EndpointsResource`] (EDS)
//!
//! These are *validated* types containing only the fields relevant to gRPC

pub(crate) mod cluster;
pub(crate) mod endpoints;
pub(crate) mod listener;
pub(crate) mod route_config;

pub(crate) use cluster::{ClusterResource, LbPolicy};
pub(crate) use endpoints::{EndpointsResource, LocalityEndpoints, ResolvedEndpoint};
pub(crate) use listener::ListenerResource;
pub(crate) use route_config::{RouteConfigResource, VirtualHostConfig};
