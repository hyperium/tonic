//! Shared XDS cache between the resource manager and Tower service layers.
//!
//! The resource manager writes validated resources into this cache, and consumers
//! use [`CacheWatch`] to receive notifications. Some consumers (e.g. the routing
//! layer) maintain their own local copies for hot-path performance.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::watch;

use crate::xds::resource::{ClusterResource, EndpointsResource, RouteConfigResource};

/// A wrapper around [`watch::Receiver`] that exposes only a single `next()`
/// method, preventing misuse of the raw watch API.
pub(crate) struct CacheWatch<T> {
    rx: watch::Receiver<Option<Arc<T>>>,
}

impl<T> CacheWatch<T> {
    fn new(mut rx: watch::Receiver<Option<Arc<T>>>) -> Self {
        // Ensure late watchers see the existing value on first next().
        rx.mark_changed();
        Self { rx }
    }

    /// Waits for the next resource update and returns it.
    ///
    /// Returns `None` if the sender was dropped (resource removed from cache).
    pub(crate) async fn next(&mut self) -> Option<Arc<T>> {
        loop {
            if self.rx.changed().await.is_err() {
                return None;
            }
            let val = self.rx.borrow_and_update().clone();
            if val.is_some() {
                return val;
            }
        }
    }
}

/// A keyed collection of [`watch`] channels for a single xDS resource type.
///
/// Each entry is lazily created on first access (watch or update) and
/// starts with `None`. Writers call [`update`](Self::update) to set the value;
/// consumers call [`watch`](Self::watch) to receive changes.
struct WatchMap<T> {
    inner: DashMap<String, watch::Sender<Option<Arc<T>>>>,
}

impl<T> WatchMap<T> {
    fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Updates the resource for the given key and notifies watchers.
    ///
    /// Lazily creates the watch channel if this is the first access for the key.
    fn update(&self, key: &str, value: Arc<T>) {
        let tx = self.ensure(key);
        tx.send_replace(Some(value));
    }

    /// Watches resource changes for the given key.
    ///
    /// Lazily creates the watch channel if it doesn't exist yet.
    fn watch(&self, key: &str) -> CacheWatch<T> {
        let tx = self.ensure(key);
        CacheWatch::new(tx.subscribe())
    }

    /// Removes the watch channel for the given key.
    ///
    /// Dropping the sender closes all watcher receivers.
    fn remove(&self, key: &str) {
        self.inner.remove(key);
    }

    /// Returns the sender for `key`, creating the watch channel if needed.
    fn ensure(&self, key: &str) -> watch::Sender<Option<Arc<T>>> {
        self.inner
            .entry(key.to_string())
            .or_insert_with(|| watch::channel(None).0)
            .value()
            .clone()
    }
}

/// Central cache for validated xDS resources.
///
/// The resource manager writes into this cache as LDS/RDS/CDS/EDS updates arrive.
/// Consumers use [`CacheWatch`] to receive notifications:
///
/// - **Routing layer**: calls [`watch_route_config`](Self::watch_route_config),
///   owns its own `ArcSwap`, and updates it when the watch fires.
/// - **Cluster layer**: calls [`watch_cluster`](Self::watch_cluster) to await
///   CDS readiness, then [`watch_endpoints`](Self::watch_endpoints) to track EDS updates.
pub(crate) struct XdsCache {
    /// Active route configuration (from LDS inline or RDS).
    route_config_tx: watch::Sender<Option<Arc<RouteConfigResource>>>,

    /// Per-cluster CDS state with readiness gating.
    clusters: WatchMap<ClusterResource>,

    /// Per-cluster EDS endpoint snapshots with readiness gating.
    endpoints: WatchMap<EndpointsResource>,
}

impl XdsCache {
    /// Creates a new empty cache with no resources.
    pub(crate) fn new() -> Self {
        let (route_config_tx, _) = watch::channel(None);
        Self {
            route_config_tx,
            clusters: WatchMap::new(),
            endpoints: WatchMap::new(),
        }
    }

    /// Updates the active route configuration and notifies all watchers.
    pub(crate) fn update_route_config(&self, config: Arc<RouteConfigResource>) {
        self.route_config_tx.send_replace(Some(config));
    }

    /// Watches route configuration changes.
    pub(crate) fn watch_route_config(&self) -> CacheWatch<RouteConfigResource> {
        CacheWatch::new(self.route_config_tx.subscribe())
    }

    /// Updates a cluster resource and notifies watchers.
    pub(crate) fn update_cluster(&self, name: &str, cluster: Arc<ClusterResource>) {
        self.clusters.update(name, cluster);
    }

    /// Removes a cluster resource and its watch channel.
    pub(crate) fn remove_cluster(&self, name: &str) {
        self.clusters.remove(name);
    }

    /// Watches cluster resource changes for a specific cluster.
    #[allow(dead_code)] // Will be used when LB policy dispatch is wired (A48).
    pub(crate) fn watch_cluster(&self, name: &str) -> CacheWatch<ClusterResource> {
        self.clusters.watch(name)
    }

    /// Updates the endpoint resource for a cluster and notifies watchers.
    pub(crate) fn update_endpoints(&self, cluster_name: &str, endpoints: Arc<EndpointsResource>) {
        self.endpoints.update(cluster_name, endpoints);
    }

    /// Watches endpoint changes for a cluster.
    pub(crate) fn watch_endpoints(&self, cluster_name: &str) -> CacheWatch<EndpointsResource> {
        self.endpoints.watch(cluster_name)
    }

    /// Removes the endpoint resource and its watch channel.
    pub(crate) fn remove_endpoints(&self, cluster_name: &str) {
        self.endpoints.remove(cluster_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::cluster::LbPolicy;

    fn make_route_config(name: &str) -> Arc<RouteConfigResource> {
        use crate::xds::resource::route_config::{
            PathSpecifierConfig, RouteConfig, RouteConfigAction, RouteConfigMatch,
            VirtualHostConfig,
        };
        Arc::new(RouteConfigResource {
            name: name.to_string(),
            virtual_hosts: vec![VirtualHostConfig {
                name: "vh1".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix("/".to_string()),
                        headers: vec![],
                        case_sensitive: true,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster("cluster-1".to_string()),
                }],
            }],
        })
    }

    fn make_cluster(name: &str, lb: LbPolicy) -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: name.to_string(),
            eds_service_name: None,
            lb_policy: lb,
            security: None,
        })
    }

    fn make_endpoints(cluster_name: &str) -> Arc<EndpointsResource> {
        use crate::client::endpoint::EndpointAddress;
        use crate::xds::resource::endpoints::{HealthStatus, LocalityEndpoints, ResolvedEndpoint};
        Arc::new(EndpointsResource {
            cluster_name: cluster_name.to_string(),
            localities: vec![LocalityEndpoints {
                locality: None,
                endpoints: vec![ResolvedEndpoint {
                    address: EndpointAddress::new("10.0.0.1", 8080),
                    health_status: HealthStatus::Healthy,
                    load_balancing_weight: 1,
                }],
                load_balancing_weight: 100,
                priority: 0,
            }],
        })
    }

    #[tokio::test]
    async fn route_config_update_notifies_watcher() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_route_config();

        cache.update_route_config(make_route_config("rc-1"));

        let val = watch.next().await.unwrap();
        assert_eq!(val.name, "rc-1");
    }

    #[tokio::test]
    async fn route_config_readiness_via_next() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_route_config();

        let handle = tokio::spawn(async move {
            let val = watch.next().await.unwrap();
            val.name.clone()
        });

        cache.update_route_config(make_route_config("rc-delayed"));
        let name = handle.await.unwrap();
        assert_eq!(name, "rc-delayed");
    }

    #[tokio::test]
    async fn cluster_watch_notifies_on_update() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_cluster("c1");

        cache.update_cluster("c1", make_cluster("c1", LbPolicy::LeastRequest));

        let val = watch.next().await.unwrap();
        assert_eq!(val.lb_policy, LbPolicy::LeastRequest);
    }

    #[tokio::test]
    async fn cluster_remove_closes_watcher() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_cluster("c1");
        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        watch.next().await; // consume

        cache.remove_cluster("c1");
        assert!(watch.next().await.is_none());
    }

    #[tokio::test]
    async fn endpoint_update_notifies_watcher() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_endpoints("c1");

        cache.update_endpoints("c1", make_endpoints("c1"));

        let val = watch.next().await.unwrap();
        assert_eq!(val.cluster_name, "c1");
        assert_eq!(val.localities.len(), 1);
    }

    #[tokio::test]
    async fn multiple_endpoint_watchers_see_same_update() {
        let cache = XdsCache::new();
        let mut watch1 = cache.watch_endpoints("c1");
        let mut watch2 = cache.watch_endpoints("c1");

        cache.update_endpoints("c1", make_endpoints("c1"));

        assert_eq!(watch1.next().await.unwrap().cluster_name, "c1");
        assert_eq!(watch2.next().await.unwrap().cluster_name, "c1");
    }

    #[tokio::test]
    async fn remove_endpoints_closes_watchers() {
        let cache = XdsCache::new();
        let mut watch = cache.watch_endpoints("c1");
        cache.update_endpoints("c1", make_endpoints("c1"));
        watch.next().await; // consume

        cache.remove_endpoints("c1");
        assert!(watch.next().await.is_none());
    }

    #[tokio::test]
    async fn cascade_cds_then_eds_readiness() {
        let cache = XdsCache::new();

        let handle = tokio::spawn({
            let mut cluster_watch = cache.watch_cluster("c1");
            let mut endpoint_watch = cache.watch_endpoints("c1");
            async move {
                let cluster = cluster_watch.next().await.unwrap();
                assert_eq!(cluster.lb_policy, LbPolicy::RoundRobin);

                let eps = endpoint_watch.next().await.unwrap();
                assert_eq!(eps.cluster_name, "c1");
            }
        });

        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        cache.update_endpoints("c1", make_endpoints("c1"));

        handle.await.unwrap();

        let cluster = cache.watch_cluster("c1").next().await.unwrap();
        assert_eq!(cluster.name, "c1");
        assert_eq!(cluster.lb_policy, LbPolicy::RoundRobin);

        let eps = cache.watch_endpoints("c1").next().await.unwrap();
        assert_eq!(eps.cluster_name, "c1");
        assert_eq!(eps.localities.len(), 1);
    }

    #[tokio::test]
    async fn late_watcher_sees_existing_value() {
        let cache = XdsCache::new();
        cache.update_route_config(make_route_config("rc-1"));
        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        cache.update_endpoints("c1", make_endpoints("c1"));

        assert!(cache.watch_route_config().next().await.is_some());
        assert!(cache.watch_cluster("c1").next().await.is_some());
        assert!(cache.watch_endpoints("c1").next().await.is_some());
    }
}
