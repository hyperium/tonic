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

//! The xDS data model layer: validated, owned representations of the raw
//! discovery-protocol resources (LDS/RDS/CDS/EDS), each implementing
//! [`xds_client::Resource`] so it can be deserialized, named, and validated
//! (gRFC A27) independently of any live xDS traffic.
//! The dependency manager assembles these into an [`crate::xds_config::XdsConfig`].

// TODO: remove once the xDS dependency manager subscribes to these resource
// types and assembles them into an XdsConfig.
#![allow(dead_code, unused_imports)]

mod cluster;
mod endpoint;
mod listener;
mod route;
mod safe_regex;
mod string_matcher;

pub(crate) use cluster::ClusterDiscovery;
pub(crate) use cluster::ClusterResource;
pub(crate) use endpoint::EndpointAddress;
pub(crate) use endpoint::EndpointsResource;
pub(crate) use endpoint::HealthStatus;
pub(crate) use endpoint::LbEndpoint;
pub(crate) use endpoint::Locality;
pub(crate) use endpoint::LocalityLbEndpoints;
pub(crate) use listener::ListenerResource;
pub(crate) use listener::RouteSource;
pub(crate) use route::DomainMatcher;
pub(crate) use route::HeaderMatchSpecifier;
pub(crate) use route::HeaderMatcher;
pub(crate) use route::PathSpecifier;
pub(crate) use route::Route;
pub(crate) use route::RouteAction;
pub(crate) use route::RouteConfigResource;
pub(crate) use route::RouteMatch;
pub(crate) use route::VirtualHost;
pub(crate) use route::WeightedCluster;
pub(crate) use safe_regex::SafeRegex;
pub(crate) use string_matcher::StringMatcher;
