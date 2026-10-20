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

pub(crate) mod circuit_breaking;
pub(crate) mod cluster;
pub(crate) mod endpoints;
pub(crate) mod hash_policy;
pub(crate) mod http_filter;
pub(crate) mod listener;
pub(crate) mod outlier_detection;
pub(crate) mod route_config;
pub(crate) mod safe_regex;
pub(crate) mod san_matcher;
pub(crate) mod security;
pub(crate) mod string_matcher;

pub(crate) use cluster::ClusterResource;
pub(crate) use endpoints::EndpointsResource;
pub(crate) use listener::ListenerResource;
pub(crate) use route_config::RouteConfigResource;
