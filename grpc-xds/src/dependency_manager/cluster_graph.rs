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

//! The cluster graph walk.
//!
//! Starting from the root clusters, [`walk`] searches depth-first through
//! aggregate clusters (gRFC A37) down to their leaf (EDS or LogicalDNS)
//! clusters and works out two things at once:
//!
//! * what each reachable cluster resolves to ([`ClusterGraph::cluster_configs`]),
//! * which watches should exist ([`ClusterGraph::required_watches`]).

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::state::WatchState;
use crate::resource::ClusterDiscovery;
use crate::resource::ClusterResource;
use crate::resource::EndpointAddress;
use crate::resource::EndpointsResource;
use crate::xds_config::AggregateClusters;
use crate::xds_config::ClusterChildren;
use crate::xds_config::ClusterConfig;
use crate::xds_config::ClusterResolutionError;
use crate::xds_config::ClusterResult;
use crate::xds_config::LeafEndpointSource;
use crate::xds_config::LeafEndpoints;

/// The maximum depth of a chain of aggregate clusters, per gRFC A37.
/// Roots are at depth 0, so a cluster reached at this depth is too deep.
const MAX_AGGREGATE_DEPTH: usize = 16;

/// Read-only access to the cached state of every watch.
///
/// `None` means that nothing is watching that name yet. The walk treats it
/// like [`WatchState::Pending`] and adds the name to
/// [`ClusterGraph::required_watches`], so the caller starts the watch.
trait CachedResources {
    /// The state of the CDS watch for cluster `name`.
    fn cluster(&self, name: &str) -> Option<&WatchState<Arc<ClusterResource>>>;
    /// The state of the EDS watch for `eds_service_name`.
    fn endpoints(&self, eds_service_name: &str) -> Option<&WatchState<Arc<EndpointsResource>>>;
    /// The state of the DNS resolution for a LogicalDNS cluster's target.
    fn dns(&self, target: &DnsTarget) -> Option<&WatchState<Arc<[EndpointAddress]>>>;
}

/// The `host:port` that a LogicalDNS cluster resolves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DnsTarget {
    host: String,
    port: u16,
}

/// The result of a [`walk`].
#[derive(Debug, Default)]
struct ClusterGraph {
    /// Every reachable cluster that has resolved, successfully or not.
    /// A cluster is left out exactly while it, or something below it, is
    /// still pending, so the caller can tell whether its roots are ready.
    cluster_configs: HashMap<String, ClusterResult>,
    /// The watches that should exist, including those below pending clusters,
    /// so that independent parts of the graph are fetched in parallel.
    required_watches: RequiredWatches,
}

/// The watches that a walk needs.
#[derive(Debug, Default, PartialEq, Eq)]
struct RequiredWatches {
    /// CDS resource names.
    clusters: HashSet<String>,
    /// EDS resource names.
    eds_services: HashSet<String>,
    /// LogicalDNS targets.
    dns_targets: HashSet<DnsTarget>,
}

/// Walks the cluster graph from `roots`: the clusters named by the selected
/// virtual host's routes, plus any held by a cluster subscription.
///
/// The roots are visited in sorted order, so the result is deterministic.
///
/// `base_notes` are the ambient LDS and RDS notes, which are added to every
/// leaf cluster's resolution note. If a leaf has no notes at all, its note is
/// the xDS node ID instead, so that the client can be found in control plane
/// logs.
fn walk<'a>(
    cache: &'a impl CachedResources,
    roots: impl IntoIterator<Item = &'a str>,
    base_notes: &'a [Arc<str>],
    node_id: Option<&str>,
) -> ClusterGraph {
    let mut walker = GraphWalker {
        cache,
        base_notes,
        node_id_note: node_id.map(|node_id| format!("xDS node ID:{node_id}").into()),
        marks: HashMap::new(),
        graph: ClusterGraph::default(),
    };
    for root in roots.into_iter().collect::<BTreeSet<_>>() {
        // A root that is too deep has already recorded its own error in `cluster_configs`.
        let _ = walker.visit(root, 0);
    }
    walker.graph
}

/// What a visited cluster tells its parent.
#[derive(Debug, Clone)]
enum LeafClusters<'a> {
    /// Something at or below the cluster hasn't arrived yet.
    Pending,
    /// The leaf clusters at or below this cluster, in priority order, and
    /// the the aggregated error notes.
    Resolved {
        names: Rc<[&'a str]>,
        notes: Rc<[Arc<str>]>,
    },
}

/// The chain of aggregate clusters through a cluster is too deep.
///
/// Every aggregate above such a cluster fails too, so this is an `Err` that
/// [`GraphWalker::aggregate`] records and then propagates with `?`.
#[derive(Debug, Clone, Copy)]
struct DepthExceeded;

impl From<DepthExceeded> for ClusterResolutionError {
    fn from(_: DepthExceeded) -> Self {
        Self::new("aggregate cluster graph exceeds max depth")
    }
}

/// A cluster's [`LeafClusters`], plus the height of the aggregate tree below
/// it.
///
#[derive(Debug, Clone)]
struct Visited<'a> {
    // Results are reused for every parent that reaches a cluster.
    leaves: LeafClusters<'a>,
    // A cluster first reached at a shallow depth may be reached again
    // through a longer path. The height lets the reused result still
    // apply the depth limit.
    height: usize,
}

impl<'a> Visited<'a> {
    const PENDING: Self = Self {
        leaves: LeafClusters::Pending,
        height: 0,
    };

    /// A leaf cluster, which is its own only leaf.
    fn leaf(name: &'a str) -> Self {
        Self {
            leaves: LeafClusters::Resolved {
                names: Rc::from([name]),
                notes: Rc::from([]),
            },
            height: 0,
        }
    }

    /// A cluster that gives its parent no leaves, and why, if there is a
    /// reason to report.
    fn no_leaves(reason: Option<Arc<str>>) -> Self {
        Self {
            leaves: LeafClusters::Resolved {
                names: Rc::from([]),
                notes: reason.into_iter().collect(),
            },
            height: 0,
        }
    }
}

/// Where a cluster is in the search.
#[derive(Debug)]
enum Mark<'a> {
    /// On the current path from a root: reaching it again closes a cycle.
    OnPath,
    /// Finished: its result is reused by every other parent that reaches it.
    Done(Visited<'a>),
}

/// The state of one walk: a recursive depth-first search over the cluster
/// graph.
///
/// The search is post-order: an aggregate's result is built from its
/// children's results after they return. It differs from a textbook DFS in a
/// few ways:
///
/// * Each cluster reached is marked [`Mark::OnPath`] while it is on the
///   current path, which detects cycles, and then [`Mark::Done`] with its
///   result, so that each cluster is computed only once per walk.
/// * Paths are limited to [`MAX_AGGREGATE_DEPTH`]. A path that is too deep
///   returns [`DepthExceeded`], which fails every aggregate above it.
/// * An edge that closes a cycle contributes no leaves, rather than failing.
/// * A pending child doesn't stop the search, so the watches of its siblings
///   are still recorded.
///
/// [`Self::visit`] does the search bookkeeping, and [`Self::visit_uncached`]
/// does the xDS work for a single cluster.
struct GraphWalker<'a, C> {
    cache: &'a C,
    base_notes: &'a [Arc<str>],
    /// The note for a leaf that has no other notes.
    node_id_note: Option<Arc<str>>,
    /// The search state of each cluster reached so far.
    marks: HashMap<&'a str, Mark<'a>>,
    graph: ClusterGraph,
}

impl<'a, C: CachedResources> GraphWalker<'a, C> {
    /// Visits cluster `name`, found `depth` aggregate levels below a root.
    ///
    /// This does only the search bookkeeping: the depth limit, the marks, and
    /// cycle detection. The cluster itself is handled by
    /// [`Self::visit_uncached`].
    fn visit(&mut self, name: &'a str, depth: usize) -> Result<Visited<'a>, DepthExceeded> {
        if depth >= MAX_AGGREGATE_DEPTH {
            return Err(DepthExceeded);
        }
        match self.marks.get(name) {
            // A cycle. The edge back into the current path contributes
            // nothing. Which edge gets cut depends on where the cycle is
            // entered, which is deterministic because roots are sorted.
            Some(Mark::OnPath) => return Ok(Visited::no_leaves(None)),
            Some(Mark::Done(visited)) if depth + visited.height >= MAX_AGGREGATE_DEPTH => {
                return Err(DepthExceeded);
            }
            Some(Mark::Done(visited)) => return Ok(visited.clone()),
            None => {}
        }
        self.marks.insert(name, Mark::OnPath);
        let result = self.visit_uncached(name, depth);
        match &result {
            Ok(visited) => self.marks.insert(name, Mark::Done(visited.clone())),
            // A cluster that is too deep on this path may be fine on a shorter
            // one.
            Err(DepthExceeded) => self.marks.remove(name),
        };
        result
    }

    /// Handles cluster `name` the first time it is reached in this walk.
    ///
    /// Records its CDS watch, reads its cached state, and then handles it by
    /// type: a leaf through [`Self::leaf`], an aggregate through
    /// [`Self::aggregate`], which continues the search into its children.
    fn visit_uncached(
        &mut self,
        name: &'a str,
        depth: usize,
    ) -> Result<Visited<'a>, DepthExceeded> {
        self.graph.required_watches.clusters.insert(name.to_owned());
        let cache = self.cache;
        let (cluster, cds_note) = match cache.cluster(name) {
            None | Some(WatchState::Pending) => return Ok(Visited::PENDING),
            Some(WatchState::Failed(reason)) => {
                self.graph.cluster_configs.insert(
                    name.to_owned(),
                    Err(ClusterResolutionError::new(reason.clone())),
                );
                // An aggregate parent skips this cluster and keeps the reason
                // in its note (gRFC A74).
                return Ok(Visited::no_leaves(Some(reason.clone())));
            }
            Some(WatchState::Ready { value, note }) => (value, note.as_ref()),
        };
        match &cluster.discovery {
            ClusterDiscovery::Eds { eds_service_name } => {
                let endpoints = cache.endpoints(eds_service_name);
                self.graph
                    .required_watches
                    .eds_services
                    .insert(eds_service_name.clone());
                Ok(self.leaf(name, cluster, cds_note, endpoints, LeafEndpointSource::Eds))
            }
            ClusterDiscovery::LogicalDns { hostname, port } => {
                let target = DnsTarget {
                    host: hostname.clone(),
                    port: *port,
                };
                let addresses = cache.dns(&target);
                self.graph.required_watches.dns_targets.insert(target);
                Ok(self.leaf(
                    name,
                    cluster,
                    cds_note,
                    addresses,
                    LeafEndpointSource::LogicalDns,
                ))
            }
            ClusterDiscovery::Aggregate { children } => {
                self.aggregate(name, cluster, cds_note, children, depth)
            }
        }
    }

    /// Records a leaf (EDS or LogicalDNS) cluster.
    ///
    /// If its endpoints failed, the cluster still resolves, with no endpoints
    /// and the reason in its note: gRFC A74 reports endpoint problems through
    /// the resolution note rather than as a cluster error.
    fn leaf<T: Clone>(
        &mut self,
        name: &'a str,
        cluster: &Arc<ClusterResource>,
        cds_note: Option<&Arc<str>>,
        endpoints: Option<&WatchState<T>>,
        to_source: fn(T) -> LeafEndpointSource,
    ) -> Visited<'a> {
        let (endpoints, endpoints_note) = match endpoints {
            None | Some(WatchState::Pending) => return Visited::PENDING,
            Some(WatchState::Ready { value, note }) => (Some(value.clone()), note.as_ref()),
            Some(WatchState::Failed(reason)) => (None, Some(reason)),
        };
        let config = ClusterConfig {
            cluster: cluster.clone(),
            children: ClusterChildren::Leaf(LeafEndpoints {
                source: endpoints.map(to_source),
                resolution_note: self.leaf_note(cds_note, endpoints_note),
            }),
        };
        self.graph
            .cluster_configs
            .insert(name.to_owned(), Ok(Arc::new(config)));
        // A leaf's note stays on its own entry and isn't passed up.
        Visited::leaf(name)
    }

    /// The LDS, RDS, CDS and endpoint notes, in that order, or the node ID if
    /// there are none.
    fn leaf_note(
        &self,
        cds_note: Option<&Arc<str>>,
        endpoints_note: Option<&Arc<str>>,
    ) -> Option<Arc<str>> {
        let notes = self.base_notes.iter().chain(cds_note).chain(endpoints_note);
        join_notes(notes).or_else(|| self.node_id_note.clone())
    }

    /// Flattens an aggregate cluster into its leaf clusters, in priority order.
    ///
    /// This is where the search recurses: each child is visited at
    /// `depth + 1`, in order.
    ///
    /// A child that failed, or that has no leaf clusters itself, is skipped
    /// and its notes are added to this aggregate's note, instead of failing
    /// the whole aggregate (gRFC A74). The aggregate fails only if no leaves
    /// remain.
    fn aggregate(
        &mut self,
        name: &'a str,
        cluster: &Arc<ClusterResource>,
        cds_note: Option<&Arc<str>>,
        children: &'a [String],
        depth: usize,
    ) -> Result<Visited<'a>, DepthExceeded> {
        let mut leaf_clusters = Vec::new();
        // A leaf reachable through several children keeps its first, highest
        // priority, position.
        let mut seen_leaves = HashSet::new();
        // Notes are deduplicated too.
        let mut notes: Vec<Arc<str>> = cds_note.into_iter().cloned().collect();
        let mut seen_notes: HashSet<Arc<str>> = notes.iter().cloned().collect();
        let mut pending = false;
        let mut height = 0;
        for child in children {
            // Every aggregate above a cluster that is too deep fails too.
            let visited = self.visit(child, depth + 1).inspect_err(|_| {
                self.graph
                    .cluster_configs
                    .insert(name.to_owned(), Err(DepthExceeded.into()));
            })?;
            height = height.max(visited.height + 1);
            match visited.leaves {
                // Keep going, so that the other children's watches start too.
                LeafClusters::Pending => pending = true,
                LeafClusters::Resolved {
                    names,
                    notes: child_notes,
                } => {
                    leaf_clusters.extend(
                        names
                            .iter()
                            .copied()
                            .filter(|leaf| seen_leaves.insert(*leaf)),
                    );
                    notes.extend(
                        child_notes
                            .iter()
                            .filter(|note| seen_notes.insert(Arc::clone(note)))
                            .cloned(),
                    );
                }
            }
        }
        if pending {
            // Left out of `cluster_configs` until everything below it arrives.
            return Ok(Visited {
                leaves: LeafClusters::Pending,
                height,
            });
        }

        let note = join_notes(&notes);
        let result = if leaf_clusters.is_empty() {
            let mut message =
                format!("aggregate cluster dependency graph for {name} has no leaf clusters");
            if let Some(note) = &note {
                message.push_str(": ");
                message.push_str(note);
            }
            Err(ClusterResolutionError::new(message))
        } else {
            let config = ClusterConfig {
                cluster: cluster.clone(),
                children: ClusterChildren::Aggregate(AggregateClusters {
                    leaf_clusters: leaf_clusters.iter().copied().map(str::to_owned).collect(),
                    resolution_note: note,
                }),
            };
            Ok(Arc::new(config))
        };
        self.graph.cluster_configs.insert(name.to_owned(), result);
        Ok(Visited {
            leaves: LeafClusters::Resolved {
                names: leaf_clusters.into(),
                notes: notes.into(),
            },
            height,
        })
    }
}

/// Joins the non-empty error notes with `"; "`.
fn join_notes<'n>(notes: impl IntoIterator<Item = &'n Arc<str>>) -> Option<Arc<str>> {
    let notes: Vec<&str> = notes
        .into_iter()
        .map(|note| note.as_ref())
        .filter(|note| !note.is_empty())
        .collect();
    (!notes.is_empty()).then(|| notes.join("; ").into())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    // ----- Fixtures -----

    /// Cached states held in plain maps. Counts cluster lookups so tests can
    /// assert that each cluster is computed once.
    #[derive(Default)]
    struct MockCache {
        clusters: HashMap<String, WatchState<Arc<ClusterResource>>>,
        endpoints: HashMap<String, WatchState<Arc<EndpointsResource>>>,
        dns: HashMap<DnsTarget, WatchState<Arc<[EndpointAddress]>>>,
        cluster_lookups: RefCell<HashMap<String, usize>>,
    }

    impl CachedResources for MockCache {
        fn cluster(&self, name: &str) -> Option<&WatchState<Arc<ClusterResource>>> {
            *self
                .cluster_lookups
                .borrow_mut()
                .entry(name.to_owned())
                .or_default() += 1;
            self.clusters.get(name)
        }

        fn endpoints(&self, eds_service_name: &str) -> Option<&WatchState<Arc<EndpointsResource>>> {
            self.endpoints.get(eds_service_name)
        }

        fn dns(&self, target: &DnsTarget) -> Option<&WatchState<Arc<[EndpointAddress]>>> {
            self.dns.get(target)
        }
    }

    impl MockCache {
        fn with_cds(mut self, name: &str, state: WatchState<Arc<ClusterResource>>) -> Self {
            self.clusters.insert(name.to_owned(), state);
            self
        }

        fn with_eds(mut self, service: &str, state: WatchState<Arc<EndpointsResource>>) -> Self {
            self.endpoints.insert(service.to_owned(), state);
            self
        }

        fn with_dns(
            mut self,
            host: &str,
            port: u16,
            state: WatchState<Arc<[EndpointAddress]>>,
        ) -> Self {
            self.dns.insert(dns_target(host, port), state);
            self
        }

        /// An EDS cluster `name`, with EDS service `name`, and endpoints.
        fn with_leaf(self, name: &str) -> Self {
            self.with_cds(name, ready(eds_cluster(name, name)))
                .with_eds(name, ready(endpoints(name)))
        }

        fn with_aggregate<const N: usize>(self, name: &str, children: [&str; N]) -> Self {
            self.with_cds(name, ready(aggregate_cluster(name, children)))
        }

        /// A chain of `levels` aggregate clusters, `{prefix}0` to
        /// `{prefix}{levels - 1}`, ending in `leaf`.
        fn with_chain(self, prefix: &str, levels: usize, leaf: &str) -> Self {
            let names: Vec<String> = (0..levels)
                .map(|i| format!("{prefix}{i}"))
                .chain([leaf.to_owned()])
                .collect();
            // Each adjacent pair is a parent and its only child.
            names.windows(2).fold(self, |cache, pair| {
                cache.with_aggregate(&pair[0], [pair[1].as_str()])
            })
        }

        /// A stack of `levels` diamonds of aggregate clusters:
        /// `D{i} = [L{i}, R{i}]` and `L{i} = R{i} = [D{i + 1}]`. The caller
        /// adds `D{levels}`, which `D0` reaches through `2^levels` paths.
        fn with_diamonds(self, levels: usize) -> Self {
            (0..levels).fold(self, |cache, i| {
                let next = format!("D{}", i + 1);
                cache
                    .with_aggregate(&format!("D{i}"), [&format!("L{i}"), &format!("R{i}")])
                    .with_aggregate(&format!("L{i}"), [&next])
                    .with_aggregate(&format!("R{i}"), [&next])
            })
        }

        fn lookups(&self, name: &str) -> usize {
            self.cluster_lookups
                .borrow()
                .get(name)
                .copied()
                .unwrap_or(0)
        }
    }

    fn ready<T>(value: T) -> WatchState<T> {
        WatchState::Ready { value, note: None }
    }

    fn ready_with_note<T>(value: T, note: &str) -> WatchState<T> {
        WatchState::Ready {
            value,
            note: Some(note.into()),
        }
    }

    fn failed<T>(reason: &str) -> WatchState<T> {
        WatchState::Failed(reason.into())
    }

    fn eds_cluster(name: &str, service: &str) -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: name.to_owned(),
            discovery: ClusterDiscovery::Eds {
                eds_service_name: service.to_owned(),
            },
        })
    }

    fn dns_cluster(name: &str, host: &str, port: u16) -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: name.to_owned(),
            discovery: ClusterDiscovery::LogicalDns {
                hostname: host.to_owned(),
                port,
            },
        })
    }

    fn aggregate_cluster<const N: usize>(name: &str, children: [&str; N]) -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: name.to_owned(),
            discovery: ClusterDiscovery::Aggregate {
                children: children.map(str::to_owned).to_vec(),
            },
        })
    }

    fn endpoints(service: &str) -> Arc<EndpointsResource> {
        Arc::new(EndpointsResource {
            cluster_name: service.to_owned(),
            localities: Vec::new(),
        })
    }

    fn addresses(host: &str, port: u16) -> Arc<[EndpointAddress]> {
        Arc::from([EndpointAddress {
            host: host.to_owned(),
            port,
        }])
    }

    fn dns_target(host: &str, port: u16) -> DnsTarget {
        DnsTarget {
            host: host.to_owned(),
            port,
        }
    }

    fn walk_roots(cache: &MockCache, roots: &[&str]) -> ClusterGraph {
        walk(cache, roots.iter().copied(), &[], None)
    }

    // ----- Assertions on a walk's result -----

    fn config<'g>(graph: &'g ClusterGraph, name: &str) -> &'g ClusterConfig {
        match graph.cluster_configs.get(name) {
            Some(Ok(config)) => config,
            other => panic!("expected cluster {name} to resolve, got {other:?}"),
        }
    }

    fn leaf<'g>(graph: &'g ClusterGraph, name: &str) -> &'g LeafEndpoints {
        match &config(graph, name).children {
            ClusterChildren::Leaf(leaf) => leaf,
            other => panic!("expected {name} to be a leaf, got {other:?}"),
        }
    }

    fn aggregate<'g>(graph: &'g ClusterGraph, name: &str) -> &'g AggregateClusters {
        match &config(graph, name).children {
            ClusterChildren::Aggregate(aggregate) => aggregate,
            other => panic!("expected {name} to be an aggregate, got {other:?}"),
        }
    }

    fn leaves<'g>(graph: &'g ClusterGraph, name: &str) -> Vec<&'g str> {
        aggregate(graph, name)
            .leaf_clusters
            .iter()
            .map(String::as_str)
            .collect()
    }

    fn error(graph: &ClusterGraph, name: &str) -> String {
        match graph.cluster_configs.get(name) {
            Some(Err(error)) => error.message().to_owned(),
            other => panic!("expected cluster {name} to fail, got {other:?}"),
        }
    }

    fn assert_depth_exceeded(graph: &ClusterGraph, name: &str) {
        assert_eq!(
            error(graph, name),
            "aggregate cluster graph exceeds max depth",
            "expected cluster {name} to exceed max depth"
        );
    }

    fn names(names: &[&str]) -> HashSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    /// What a leaf cluster is expected to resolve to.
    #[derive(Debug)]
    enum ExpectedLeaf {
        /// Left out of `cluster_configs` until its endpoints arrive.
        Pending,
        /// Resolved, with or without endpoints, and with this note.
        Resolved {
            has_endpoints: bool,
            note: Option<&'static str>,
        },
    }

    impl ExpectedLeaf {
        /// Checks leaf cluster `name`, and returns its endpoints, if any.
        fn check<'g>(&self, graph: &'g ClusterGraph, name: &str) -> Option<&'g LeafEndpointSource> {
            match self {
                Self::Pending => {
                    assert!(!graph.cluster_configs.contains_key(name), "{self:?}");
                    None
                }
                Self::Resolved {
                    has_endpoints,
                    note,
                } => {
                    let leaf = leaf(graph, name);
                    assert_eq!(leaf.source.is_some(), *has_endpoints, "{self:?}");
                    assert_eq!(leaf.resolution_note.as_deref(), *note, "{self:?}");
                    leaf.source.as_ref()
                }
            }
        }
    }

    // ----- Leaf clusters -----

    #[test]
    fn eds_leaf_follows_endpoints_state() {
        for (endpoints_state, expected) in [
            (None, ExpectedLeaf::Pending),
            (Some(WatchState::Pending), ExpectedLeaf::Pending),
            (
                Some(ready(endpoints("a"))),
                ExpectedLeaf::Resolved {
                    has_endpoints: true,
                    note: None,
                },
            ),
            (
                Some(ready_with_note(endpoints("a"), "EDS a: stream broke")),
                ExpectedLeaf::Resolved {
                    has_endpoints: true,
                    note: Some("EDS a: stream broke"),
                },
            ),
            // Endpoint failures are notes, not cluster errors.
            (
                Some(failed("EDS a: does not exist")),
                ExpectedLeaf::Resolved {
                    has_endpoints: false,
                    note: Some("EDS a: does not exist"),
                },
            ),
        ] {
            let mut cache = MockCache::default().with_cds("A", ready(eds_cluster("A", "a")));
            if let Some(state) = endpoints_state {
                cache = cache.with_eds("a", state);
            }
            let graph = walk_roots(&cache, &["A"]);

            assert_eq!(graph.required_watches.eds_services, names(&["a"]));
            if let Some(source) = expected.check(&graph, "A") {
                assert!(matches!(source, LeafEndpointSource::Eds(_)), "{source:?}");
            }
        }
    }

    #[test]
    fn logical_dns_leaf_follows_dns_state() {
        for (dns_state, expected) in [
            (None, ExpectedLeaf::Pending),
            (Some(WatchState::Pending), ExpectedLeaf::Pending),
            (
                Some(ready(addresses("10.0.0.1", 443))),
                ExpectedLeaf::Resolved {
                    has_endpoints: true,
                    note: None,
                },
            ),
            (
                Some(ready_with_note(
                    addresses("10.0.0.1", 443),
                    "DNS d.example.com: re-resolution failed",
                )),
                ExpectedLeaf::Resolved {
                    has_endpoints: true,
                    note: Some("DNS d.example.com: re-resolution failed"),
                },
            ),
            (
                Some(failed("DNS d.example.com: timed out")),
                ExpectedLeaf::Resolved {
                    has_endpoints: false,
                    note: Some("DNS d.example.com: timed out"),
                },
            ),
        ] {
            let mut cache =
                MockCache::default().with_cds("D", ready(dns_cluster("D", "d.example.com", 443)));
            if let Some(state) = dns_state {
                cache = cache.with_dns("d.example.com", 443, state);
            }
            let graph = walk_roots(&cache, &["D"]);

            assert_eq!(
                graph.required_watches.dns_targets,
                HashSet::from([dns_target("d.example.com", 443)])
            );
            if let Some(source) = expected.check(&graph, "D") {
                assert!(
                    matches!(source, LeafEndpointSource::LogicalDns(addresses) if addresses[0].host == "10.0.0.1"),
                    "{source:?}"
                );
            }
        }
    }

    #[test]
    fn missing_or_failed_cds_resource() {
        let graph = walk_roots(&MockCache::default(), &["A"]);
        assert!(graph.cluster_configs.is_empty());
        assert_eq!(graph.required_watches.clusters, names(&["A"]));

        let cache = MockCache::default().with_cds("A", WatchState::Pending);
        assert!(walk_roots(&cache, &["A"]).cluster_configs.is_empty());

        let cache = MockCache::default().with_cds("A", failed("CDS A: does not exist"));
        let graph = walk_roots(&cache, &["A"]);
        assert_eq!(error(&graph, "A"), "CDS A: does not exist");
    }

    #[test]
    fn leaf_note_joins_lds_rds_cds_and_endpoint_notes_in_order() {
        let cache = MockCache::default()
            .with_cds("A", ready_with_note(eds_cluster("A", "a"), "cds"))
            .with_eds("a", ready_with_note(endpoints("a"), "eds"));
        let base_notes: [Arc<str>; 3] = ["lds".into(), "".into(), "rds".into()];
        let graph = walk(&cache, ["A"], &base_notes, Some("node-1"));
        assert_eq!(
            leaf(&graph, "A").resolution_note.as_deref(),
            Some("lds; rds; cds; eds")
        );
    }

    #[test]
    fn leaf_note_falls_back_to_node_id() {
        let cache = MockCache::default().with_leaf("A");
        let graph = walk(&cache, ["A"], &[], Some("node-1"));
        assert_eq!(
            leaf(&graph, "A").resolution_note.as_deref(),
            Some("xDS node ID:node-1")
        );

        let graph = walk(&cache, ["A"], &[], None);
        assert_eq!(leaf(&graph, "A").resolution_note.as_deref(), None);
    }

    // ----- Aggregate clusters -----

    #[test]
    fn aggregate_flattens_children_in_priority_order_without_duplicates() {
        // X = [A, Y], Y = [B, A]: A keeps its first position in X.
        let cache = MockCache::default()
            .with_leaf("A")
            .with_leaf("B")
            .with_aggregate("X", ["A", "Y"])
            .with_aggregate("Y", ["B", "A"]);
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(leaves(&graph, "X"), ["A", "B"]);
        assert_eq!(leaves(&graph, "Y"), ["B", "A"]);
        // Every reachable cluster has its own entry.
        leaf(&graph, "A");
        leaf(&graph, "B");
        assert_eq!(graph.cluster_configs.len(), 4);
    }

    #[test]
    fn aggregate_skips_failed_children() {
        let cache = MockCache::default()
            .with_leaf("A")
            .with_cds("B", failed("CDS B: does not exist"))
            .with_aggregate("X", ["A", "B"]);
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(leaves(&graph, "X"), ["A"]);
        assert_eq!(
            aggregate(&graph, "X").resolution_note.as_deref(),
            Some("CDS B: does not exist")
        );
        assert_eq!(error(&graph, "B"), "CDS B: does not exist");
    }

    #[test]
    fn aggregate_without_leaves_fails_with_the_reasons() {
        let cache = MockCache::default()
            .with_cds("A", failed("CDS A: does not exist"))
            .with_cds("B", failed("CDS B: invalid"))
            .with_aggregate("X", ["A", "B"]);
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(
            error(&graph, "X"),
            "aggregate cluster dependency graph for X has no leaf clusters: \
             CDS A: does not exist; CDS B: invalid"
        );

        let cache = MockCache::default().with_aggregate("X", []);
        let graph = walk_roots(&cache, &["X"]);
        assert_eq!(
            error(&graph, "X"),
            "aggregate cluster dependency graph for X has no leaf clusters"
        );
    }

    #[test]
    fn aggregate_note_collects_own_note_skipped_children_and_child_aggregates() {
        // X (with its own CDS note) = [Y, B], Y = [C, A]; B and C fail. A's
        // own note stays on A.
        let cache = MockCache::default()
            .with_cds(
                "X",
                ready_with_note(aggregate_cluster("X", ["Y", "B"]), "CDS X: stale"),
            )
            .with_aggregate("Y", ["C", "A"])
            .with_cds("A", ready(eds_cluster("A", "a")))
            .with_eds("a", failed("EDS a: does not exist"))
            .with_cds("B", failed("CDS B: does not exist"))
            .with_cds("C", failed("CDS C: does not exist"));
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(leaves(&graph, "X"), ["A"]);
        assert_eq!(
            aggregate(&graph, "X").resolution_note.as_deref(),
            Some("CDS X: stale; CDS C: does not exist; CDS B: does not exist")
        );
        assert_eq!(
            aggregate(&graph, "Y").resolution_note.as_deref(),
            Some("CDS C: does not exist")
        );
        // A leaf without endpoints still counts as a leaf.
        assert!(leaf(&graph, "A").source.is_none());
    }

    #[test]
    fn child_aggregate_without_leaves_is_skipped_with_its_notes() {
        let cache = MockCache::default()
            .with_leaf("A")
            .with_aggregate("X", ["Y", "A"])
            .with_aggregate("Y", ["B"])
            .with_cds("B", failed("CDS B: does not exist"));
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(
            error(&graph, "Y"),
            "aggregate cluster dependency graph for Y has no leaf clusters: CDS B: does not exist"
        );
        assert_eq!(leaves(&graph, "X"), ["A"]);
        // X gets Y's notes, not Y's error message.
        assert_eq!(
            aggregate(&graph, "X").resolution_note.as_deref(),
            Some("CDS B: does not exist")
        );
    }

    #[test]
    fn pending_child_blocks_aggregate_but_siblings_are_still_watched() {
        // B has no CDS watch yet, and C's endpoints haven't arrived.
        let cache = MockCache::default()
            .with_leaf("A")
            .with_cds("C", ready(eds_cluster("C", "c")))
            .with_aggregate("X", ["A", "B", "C"]);
        let graph = walk_roots(&cache, &["X"]);

        assert!(!graph.cluster_configs.contains_key("X"));
        assert_eq!(
            graph.required_watches,
            RequiredWatches {
                clusters: names(&["X", "A", "B", "C"]),
                eds_services: names(&["A", "c"]),
                dns_targets: HashSet::new(),
            }
        );
    }

    #[test]
    fn shared_leaf_is_not_stolen_from_aggregate() {
        // Routes to [L, X], X = [L]. Being referenced by X
        // shouldn't prevent L from being a leaf.
        let cache = MockCache::default()
            .with_leaf("L")
            .with_aggregate("X", ["L"]);
        let graph = walk_roots(&cache, &["L", "X"]);

        assert_eq!(leaves(&graph, "X"), ["L"]);
        leaf(&graph, "L");
    }

    #[test]
    fn each_cluster_is_computed_once() {
        // Without memoization, D7 would be visited 2^7 times.
        let cache = MockCache::default().with_diamonds(7).with_leaf("D7");
        let graph = walk_roots(&cache, &["D0"]);

        assert_eq!(leaves(&graph, "D0"), ["D7"]);
        assert_eq!(cache.lookups("D7"), 1);
        assert_eq!(cache.lookups("D4"), 1);
    }

    #[test]
    fn notes_reached_through_several_paths_appear_once() {
        // Every path from D0 reaches D7's failure. Repeating it once per path
        // would put it in D0's error 2^7 times.
        let cache = MockCache::default()
            .with_diamonds(7)
            .with_cds("D7", failed("CDS D7: does not exist"));
        let graph = walk_roots(&cache, &["D0"]);
        assert_eq!(
            error(&graph, "D0"),
            "aggregate cluster dependency graph for D0 has no leaf clusters: \
             CDS D7: does not exist"
        );

        // The same goes for the ambient note of a shared aggregate that still
        // has leaves.
        let cache = MockCache::default()
            .with_diamonds(1)
            .with_cds(
                "D1",
                ready_with_note(aggregate_cluster("D1", ["A"]), "CDS D1: stale"),
            )
            .with_leaf("A");
        let graph = walk_roots(&cache, &["D0"]);
        assert_eq!(leaves(&graph, "D0"), ["A"]);
        assert_eq!(
            aggregate(&graph, "D0").resolution_note.as_deref(),
            Some("CDS D1: stale")
        );
    }

    // ----- Cycles and depth -----

    #[test]
    fn self_cycle_has_no_leaves() {
        let cache = MockCache::default().with_aggregate("X", ["X"]);
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(
            error(&graph, "X"),
            "aggregate cluster dependency graph for X has no leaf clusters"
        );
    }

    #[test]
    fn cycle_is_cut_where_it_is_entered() {
        // X = [Y, L], Y = [X].
        let cache = MockCache::default()
            .with_leaf("L")
            .with_aggregate("X", ["Y", "L"])
            .with_aggregate("Y", ["X"]);

        // Entered at X: the edge Y -> X is cut, so Y has no leaves.
        let graph = walk_roots(&cache, &["X"]);
        assert_eq!(leaves(&graph, "X"), ["L"]);
        assert!(error(&graph, "Y").contains("for Y has no leaf clusters"));

        // Entered at Y: the edge X -> Y is cut instead.
        let graph = walk_roots(&cache, &["Y"]);
        assert_eq!(leaves(&graph, "Y"), ["L"]);
        assert_eq!(leaves(&graph, "X"), ["L"]);

        // With both as roots, the result doesn't depend on the order given.
        let x_first = walk_roots(&cache, &["X", "Y"]);
        let y_first = walk_roots(&cache, &["Y", "X"]);
        assert_eq!(leaves(&x_first, "X"), leaves(&y_first, "X"));
        assert_eq!(error(&x_first, "Y"), error(&y_first, "Y"));
    }

    #[test]
    fn depth_limit_is_16_aggregate_levels() {
        // 15 levels of aggregates put the leaf at depth 15: allowed.
        let cache = MockCache::default().with_leaf("L").with_chain("A", 15, "L");
        let graph = walk_roots(&cache, &["A0"]);
        assert_eq!(leaves(&graph, "A0"), ["L"]);

        // 16 levels put the leaf at depth 16: every aggregate above it fails,
        // and the leaf isn't watched.
        let cache = MockCache::default().with_leaf("L").with_chain("A", 16, "L");
        let graph = walk_roots(&cache, &["A0"]);
        for i in 0..16 {
            assert_depth_exceeded(&graph, &format!("A{i}"));
        }
        assert!(!graph.cluster_configs.contains_key("L"));
        assert!(!graph.required_watches.clusters.contains("L"));
    }

    #[test]
    fn memoized_cluster_reached_through_a_deeper_path_exceeds_depth() {
        // M0..M13 is a chain of 14 aggregates over leaf L. Reached at depth 1
        // through `short = [M0]`, L is at depth 15, which is allowed. Reached
        // at depth 2 through `deep = [deep1]`, `deep1 = [M0]`, L is at depth 16.
        // The roots are named so that each order is tried.
        for (short, deep) in [("a", "z"), ("z", "a")] {
            let deep1 = format!("{deep}1");
            let cache = MockCache::default()
                .with_leaf("L")
                .with_chain("M", 14, "L")
                .with_aggregate(short, ["M0"])
                .with_aggregate(deep, [&deep1])
                .with_aggregate(&deep1, ["M0"]);
            let graph = walk_roots(&cache, &[short, deep]);

            assert_eq!(leaves(&graph, short), ["L"], "short root {short}");
            assert_eq!(leaves(&graph, "M0"), ["L"], "short root {short}");
            assert_depth_exceeded(&graph, deep);
            assert_depth_exceeded(&graph, &deep1);
        }
    }

    // ----- Roots -----

    #[test]
    fn root_is_left_out_of_cluster_configs_exactly_while_pending() {
        // The caller decides whether to publish by checking its route
        // clusters against `cluster_configs`, so this must hold however a root
        // is reached. Roots are sorted, so Y is first reached as X's child.
        let cache = MockCache::default()
            .with_leaf("A")
            .with_aggregate("X", ["Y"])
            .with_aggregate("Y", ["B"]);
        let graph = walk_roots(&cache, &["Y", "A", "X"]);
        assert!(graph.cluster_configs.contains_key("A"));
        assert!(!graph.cluster_configs.contains_key("X"));
        assert!(!graph.cluster_configs.contains_key("Y"));
        assert_eq!(
            graph.required_watches.clusters,
            names(&["A", "B", "X", "Y"])
        );

        let cache = cache.with_leaf("B");
        let graph = walk_roots(&cache, &["Y", "A", "X"]);
        assert_eq!(leaves(&graph, "X"), ["B"]);
        assert_eq!(leaves(&graph, "Y"), ["B"]);
        leaf(&graph, "A");
    }

    #[test]
    fn required_watches_cover_every_kind_of_cluster() {
        let cache = MockCache::default()
            .with_aggregate("X", ["A", "D"])
            .with_cds("A", ready(eds_cluster("A", "a-service")))
            .with_cds("D", ready(dns_cluster("D", "d.example.com", 8080)));
        let graph = walk_roots(&cache, &["X"]);

        assert_eq!(
            graph.required_watches,
            RequiredWatches {
                clusters: names(&["X", "A", "D"]),
                eds_services: names(&["a-service"]),
                dns_targets: HashSet::from([dns_target("d.example.com", 8080)]),
            }
        );
    }
}
