/*
 *
 * Copyright 2026 gRPC authors.
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

//! The xDS dependency manager (gRFC A74).
//!
//! The dependency manager watches a channel's Listener, RouteConfiguration,
//! Cluster and Endpoints resources (and DNS for LogicalDNS clusters), and
//! publishes an [`XdsConfig`](crate::xds_config::XdsConfig) snapshot whenever
//! a consistent set of them has arrived.
//!
//! So far this holds the pure building blocks:
//!
//! * `state` folds each watch's events into a cached state.
//! * `cluster_graph` walks from the root clusters through aggregate clusters,
//!   and decides what to publish and what to watch.

// TODO: remove once the dependency manager's actor uses these.
#![cfg_attr(not(test), expect(dead_code))]

mod cluster_graph;
mod state;
