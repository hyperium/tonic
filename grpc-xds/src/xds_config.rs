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

//! [`XdsConfig`]: the atomic xDS configuration snapshot for a channel.
//!
//! Per gRFC A74, it bundles everything needed to route and load balance a
//! single RPC -- a Listener, its RouteConfiguration, and every reachable
//! Cluster and its endpoints -- into one immutable value, so a config
//! update is atomic and never exposes a partial mix of old and new state.

// TODO: remove once the xDS dependency manager is implemented.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::resource::ClusterResource;
use crate::resource::EndpointAddress;
use crate::resource::EndpointsResource;
use crate::resource::ListenerResource;
use crate::resource::RouteConfigResource;
use crate::resource::RouteSource;
use crate::resource::VirtualHost;

/// The atomic xDS configuration snapshot for a channel.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct XdsConfig {
    /// The channel's Listener resource (LDS).
    pub(crate) listener: Arc<ListenerResource>,
    /// The Listener's resolved RouteConfiguration: either fetched via RDS or
    /// embedded inline in the Listener (see
    /// [`crate::resource::RouteSource`]).
    pub(crate) route_config: Arc<RouteConfigResource>,
    /// Index of the virtual host selected for this channel's data-plane
    /// authority. Kept as an index so it can safely refer into the immutable
    /// `route_config` without a self-referential borrow.
    virtual_host_index: usize,
    /// Every cluster transitively reachable from [`Self::virtual_host`], keyed
    /// by cluster name. Includes aggregate clusters' descendants.
    ///
    /// A cluster that failed to resolve (CDS/EDS validation failure, NACK,
    /// DNS lookup failure, cyclic aggregate reference, ...) is still present
    /// here with an `Err` value rather than omitted, per gRFC A74,
    /// so callers can distinguish "still loading" from "failed".
    pub(crate) clusters: HashMap<String, ClusterResult>,
}

impl XdsConfig {
    /// Constructs a snapshot for a selected virtual host.
    ///
    /// Returns `None` if `virtual_host_index` is invalid, or if `route_config`
    /// is not the route configuration the listener actually points at: the
    /// same allocation for an inline config, or the same name for RDS.
    pub(crate) fn try_new(
        listener: Arc<ListenerResource>,
        route_config: Arc<RouteConfigResource>,
        virtual_host_index: usize,
        clusters: HashMap<String, ClusterResult>,
    ) -> Option<Self> {
        route_config.virtual_hosts.get(virtual_host_index)?;
        match &listener.route_source {
            RouteSource::Inline(inline) if !Arc::ptr_eq(inline, &route_config) => return None,
            RouteSource::Rds(name) if *name != route_config.name => return None,
            _ => {}
        }
        Some(Self {
            listener,
            route_config,
            virtual_host_index,
            clusters,
        })
    }

    /// Returns the single virtual host selected for this channel.
    pub(crate) fn virtual_host(&self) -> &VirtualHost {
        self.route_config
            .virtual_hosts
            .get(self.virtual_host_index)
            .expect("XdsConfig constructor validates virtual_host_index")
    }
}

/// Resolution result for a single cluster: either its fully resolved
/// config, or the error that prevented it from resolving.
pub(crate) type ClusterResult = Result<Arc<ClusterConfig>, ClusterResolutionError>;

/// A cluster-scoped dependency-resolution failure suitable for reporting to
/// the data plane without exposing an xDS transport-layer error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClusterResolutionError {
    message: Arc<str>,
}

impl ClusterResolutionError {
    pub(crate) fn new(message: impl Into<Arc<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ClusterResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClusterResolutionError {}

/// A single resolved cluster: its static CDS configuration plus its
/// dynamically resolved children.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct ClusterConfig {
    /// The validated CDS resource itself (name, discovery mechanism, ...).
    pub(crate) cluster: Arc<ClusterResource>,
    /// This cluster's dynamically resolved children.
    pub(crate) children: ClusterChildren,
}

/// A cluster's dynamically resolved children.
#[derive(Debug, Clone)]
pub(crate) enum ClusterChildren {
    /// A leaf cluster (EDS or LogicalDNS): its resolved endpoints.
    Leaf(LeafEndpoints),
    /// An aggregate cluster (gRFC A37): the fully resolved list of leaf
    /// cluster names to fall over across, in priority order.
    Aggregate(AggregateClusters),
}

/// Resolved aggregate-cluster dependencies.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct AggregateClusters {
    pub(crate) leaf_clusters: Vec<String>,
    /// Ambient LDS/RDS/CDS errors associated with this aggregate branch.
    pub(crate) resolution_note: Option<Arc<str>>,
}

/// A leaf cluster's resolved endpoints.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct LeafEndpoints {
    /// A leaf cluster's resolved endpoints, if any have been resolved yet.
    ///
    /// `None` while the first resolution attempt is still outstanding or has
    /// only ever failed; see `resolution_note` for why.
    pub(crate) source: Option<LeafEndpointSource>,
    /// Ambient LDS/RDS/CDS/EDS or DNS diagnostic information for this branch.
    ///
    /// Unlike the `Err` side of [`ClusterResult`], a note does not mean the
    /// cluster failed: `source` may still hold stale but valid endpoints.
    pub(crate) resolution_note: Option<Arc<str>>,
}

/// The origin of a leaf cluster's endpoints.
#[derive(Debug, Clone)]
pub(crate) enum LeafEndpointSource {
    /// Resolved via EDS: the validated ClusterLoadAssignment resource.
    Eds(Arc<EndpointsResource>),
    /// Resolved via DNS resolution of a LogicalDNS cluster's target.
    LogicalDns(Arc<[EndpointAddress]>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::DomainMatcher;

    fn route_config() -> Arc<RouteConfigResource> {
        Arc::new(RouteConfigResource {
            name: "routes".into(),
            virtual_hosts: vec![
                VirtualHost {
                    name: "first".into(),
                    domains: vec![DomainMatcher::Exact("first.example.com".into())],
                    routes: Vec::new(),
                },
                VirtualHost {
                    name: "second".into(),
                    domains: vec![DomainMatcher::Exact("second.example.com".into())],
                    routes: Vec::new(),
                },
            ],
        })
    }

    fn inline_listener(route_config: Arc<RouteConfigResource>) -> Arc<ListenerResource> {
        Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Inline(route_config),
        })
    }

    #[test]
    fn selected_virtual_host_uses_route_config_index() {
        let route_config = route_config();
        let listener = inline_listener(Arc::clone(&route_config));
        let config = XdsConfig::try_new(
            Arc::clone(&listener),
            Arc::clone(&route_config),
            1,
            HashMap::new(),
        )
        .expect("valid selected virtual host");

        assert_eq!(config.virtual_host().name, "second");
        let RouteSource::Inline(inline) = &listener.route_source else {
            panic!("expected inline route config");
        };
        assert!(Arc::ptr_eq(inline, &config.route_config));
    }

    #[test]
    fn selected_virtual_host_rejects_invalid_index() {
        let route_config = route_config();
        let listener = inline_listener(Arc::clone(&route_config));
        assert!(XdsConfig::try_new(listener, route_config, 2, HashMap::new()).is_none());
    }

    #[test]
    fn selected_virtual_host_rejects_different_inline_allocation() {
        let listener_route_config = route_config();
        let snapshot_route_config = route_config();
        let listener = inline_listener(listener_route_config);
        assert!(XdsConfig::try_new(listener, snapshot_route_config, 0, HashMap::new()).is_none());
    }

    #[test]
    fn selected_virtual_host_accepts_matching_rds_name() {
        let route_config = route_config();
        let listener = Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Rds("routes".into()),
        });
        assert!(XdsConfig::try_new(listener, route_config, 0, HashMap::new()).is_some());
    }

    #[test]
    fn selected_virtual_host_rejects_mismatched_rds_name() {
        let route_config = route_config();
        let listener = Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Rds("other-routes".into()),
        });
        assert!(XdsConfig::try_new(listener, route_config, 0, HashMap::new()).is_none());
    }
}
