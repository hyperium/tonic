//! Shared XDS cache between the resource manager and Tower service layers.
//!
//! The resource manager writes validated resources into this cache, and consumers
//! subscribe to [`tokio::sync::watch`]-based notifications. Some consumers (e.g.
//! the routing layer) maintain their own local copies for hot-path performance.
//!
//! All channels use `watch<Option<Arc<T>>>`:
//! - `None` = resource not yet available (not ready)
//! - `Some(resource)` = resource available (ready)
//!
//! Consumers use `rx.wait_for(|v| v.is_some())` to await readiness.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::watch;

use crate::xds::resource::{ClusterResource, EndpointsResource, RouteConfigResource};

/// A keyed collection of [`watch`] channels for a single xDS resource type.
///
/// Each entry is lazily created on first access (subscribe or update) and
/// starts with `None`. Writers call [`update`](Self::update) to set the value;
/// consumers call [`subscribe`](Self::subscribe) and await changes.
struct WatchMap<T> {
    inner: DashMap<String, watch::Sender<Option<Arc<T>>>>,
}

impl<T> WatchMap<T> {
    fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Updates the resource for the given key and notifies subscribers.
    ///
    /// Lazily creates the watch channel if this is the first access for the key.
    fn update(&self, key: &str, value: Arc<T>) {
        let tx = self.ensure(key);
        tx.send_replace(Some(value));
    }

    /// Subscribes to resource changes for the given key.
    ///
    /// Lazily creates the watch channel if it doesn't exist yet.
    fn subscribe(&self, key: &str) -> watch::Receiver<Option<Arc<T>>> {
        let tx = self.ensure(key);
        tx.subscribe()
    }

    /// Removes the watch channel for the given key.
    ///
    /// Dropping the sender closes all subscriber receivers.
    fn remove(&self, key: &str) {
        self.inner.remove(key);
    }

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
/// Consumers subscribe to watch channels to receive notifications:
///
/// - **Routing layer**: subscribes to [`subscribe_route_config`](Self::subscribe_route_config),
///   owns its own `ArcSwap`, and updates it when the watch fires.
/// - **Cluster layer**: subscribes to [`subscribe_cluster`](Self::subscribe_cluster) to await
///   CDS readiness, then [`subscribe_endpoints`](Self::subscribe_endpoints) to track EDS updates.
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

    /// Updates the active route configuration and notifies all subscribers.
    pub(crate) fn update_route_config(&self, config: Arc<RouteConfigResource>) {
        self.route_config_tx.send_replace(Some(config));
    }

    /// Subscribes to route configuration changes.
    pub(crate) fn subscribe_route_config(
        &self,
    ) -> watch::Receiver<Option<Arc<RouteConfigResource>>> {
        self.route_config_tx.subscribe()
    }

    /// Updates a cluster resource and notifies subscribers.
    pub(crate) fn update_cluster(&self, name: &str, cluster: Arc<ClusterResource>) {
        self.clusters.update(name, cluster);
    }

    /// Removes a cluster resource and its watch channel.
    pub(crate) fn remove_cluster(&self, name: &str) {
        self.clusters.remove(name);
    }

    /// Subscribes to cluster resource changes for a specific cluster.
    pub(crate) fn subscribe_cluster(
        &self,
        name: &str,
    ) -> watch::Receiver<Option<Arc<ClusterResource>>> {
        self.clusters.subscribe(name)
    }

    /// Updates the endpoint resource for a cluster and notifies subscribers.
    pub(crate) fn update_endpoints(&self, cluster_name: &str, endpoints: Arc<EndpointsResource>) {
        self.endpoints.update(cluster_name, endpoints);
    }

    /// Subscribes to endpoint changes for a cluster.
    pub(crate) fn subscribe_endpoints(
        &self,
        cluster_name: &str,
    ) -> watch::Receiver<Option<Arc<EndpointsResource>>> {
        self.endpoints.subscribe(cluster_name)
    }

    /// Removes the endpoint resource and its watch channel.
    pub(crate) fn remove_endpoints(&self, cluster_name: &str) {
        self.endpoints.remove(cluster_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::LbPolicy;

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

    #[test]
    fn new_cache_has_no_route_config() {
        let cache = XdsCache::new();
        let rx = cache.subscribe_route_config();
        assert!(rx.borrow().is_none());
    }

    #[tokio::test]
    async fn route_config_update_notifies_subscriber() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_route_config();

        cache.update_route_config(make_route_config("rc-1"));

        rx.changed().await.unwrap();
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().name, "rc-1");
    }

    #[tokio::test]
    async fn route_config_readiness_via_wait_for() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_route_config();

        let handle = tokio::spawn(async move {
            rx.wait_for(|v| v.is_some()).await.unwrap();
            rx.borrow().as_ref().unwrap().name.clone()
        });

        cache.update_route_config(make_route_config("rc-delayed"));
        let name = handle.await.unwrap();
        assert_eq!(name, "rc-delayed");
    }

    #[tokio::test]
    async fn cluster_subscribe_notifies_on_update() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_cluster("c1");
        assert!(rx.borrow().is_none());

        cache.update_cluster("c1", make_cluster("c1", LbPolicy::LeastRequest));

        rx.changed().await.unwrap();
        let val = rx.borrow_and_update();
        assert_eq!(val.as_ref().unwrap().lb_policy, LbPolicy::LeastRequest);
    }

    #[tokio::test]
    async fn cluster_readiness_via_wait_for() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_cluster("c1");

        let handle = tokio::spawn(async move {
            rx.wait_for(|v| v.is_some()).await.unwrap();
            rx.borrow().as_ref().unwrap().name.clone()
        });

        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        let name = handle.await.unwrap();
        assert_eq!(name, "c1");
    }

    #[tokio::test]
    async fn cluster_remove_closes_subscriber() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_cluster("c1");
        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        rx.changed().await.unwrap();
        rx.borrow_and_update(); // consume

        cache.remove_cluster("c1");
        assert!(rx.changed().await.is_err());
    }

    #[tokio::test]
    async fn endpoint_update_notifies_subscriber() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_endpoints("c1");

        cache.update_endpoints("c1", make_endpoints("c1"));

        rx.changed().await.unwrap();
        let val = rx.borrow_and_update();
        let resource = val.as_ref().unwrap();
        assert_eq!(resource.cluster_name, "c1");
        assert_eq!(resource.localities.len(), 1);
    }

    #[tokio::test]
    async fn endpoint_readiness_via_wait_for() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_endpoints("c1");

        let handle = tokio::spawn(async move {
            rx.wait_for(|v| v.is_some()).await.unwrap();
            rx.borrow().as_ref().unwrap().cluster_name.clone()
        });

        cache.update_endpoints("c1", make_endpoints("c1"));
        let name = handle.await.unwrap();
        assert_eq!(name, "c1");
    }

    #[tokio::test]
    async fn multiple_endpoint_subscribers_see_same_update() {
        let cache = XdsCache::new();
        let mut rx1 = cache.subscribe_endpoints("c1");
        let mut rx2 = cache.subscribe_endpoints("c1");

        cache.update_endpoints("c1", make_endpoints("c1"));

        rx1.changed().await.unwrap();
        rx2.changed().await.unwrap();
        assert_eq!(
            rx1.borrow_and_update().as_ref().unwrap().cluster_name,
            "c1"
        );
        assert_eq!(
            rx2.borrow_and_update().as_ref().unwrap().cluster_name,
            "c1"
        );
    }

    #[tokio::test]
    async fn remove_endpoints_closes_subscribers() {
        let cache = XdsCache::new();
        let mut rx = cache.subscribe_endpoints("c1");
        cache.update_endpoints("c1", make_endpoints("c1"));
        rx.changed().await.unwrap();
        rx.borrow_and_update(); // consume

        cache.remove_endpoints("c1");
        assert!(rx.changed().await.is_err());
    }

    #[tokio::test]
    async fn cascade_cds_then_eds_readiness() {
        let cache = XdsCache::new();

        let handle = tokio::spawn({
            let mut cluster_rx = cache.subscribe_cluster("c1");
            let mut endpoint_rx = cache.subscribe_endpoints("c1");
            async move {
                cluster_rx.wait_for(|v| v.is_some()).await.unwrap();
                let cluster = cluster_rx.borrow().as_ref().unwrap().clone();
                assert_eq!(cluster.lb_policy, LbPolicy::RoundRobin);

                endpoint_rx.wait_for(|v| v.is_some()).await.unwrap();
                let eps = endpoint_rx.borrow().as_ref().unwrap().clone();
                assert_eq!(eps.cluster_name, "c1");
            }
        });

        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        cache.update_endpoints("c1", make_endpoints("c1"));

        handle.await.unwrap();
    }

    #[test]
    fn late_subscriber_sees_existing_value() {
        let cache = XdsCache::new();
        cache.update_route_config(make_route_config("rc-1"));
        cache.update_cluster("c1", make_cluster("c1", LbPolicy::RoundRobin));
        cache.update_endpoints("c1", make_endpoints("c1"));

        assert!(cache.subscribe_route_config().borrow().is_some());
        assert!(cache.subscribe_cluster("c1").borrow().is_some());
        assert!(cache.subscribe_endpoints("c1").borrow().is_some());
    }
}
