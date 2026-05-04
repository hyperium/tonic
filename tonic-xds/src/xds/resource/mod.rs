// TODO: remove once A48 (least-request LB) and priority LB consume all fields.
#![allow(dead_code)]
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
pub(crate) mod outlier_detection;
pub(crate) mod route_config;
pub(crate) mod san_matcher;
pub(crate) mod security;
pub(crate) mod string_matcher;

pub(crate) use cluster::ClusterResource;
pub(crate) use endpoints::EndpointsResource;
pub(crate) use listener::ListenerResource;
pub(crate) use route_config::RouteConfigResource;
