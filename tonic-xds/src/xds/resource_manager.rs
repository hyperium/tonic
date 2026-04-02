//! xDS resource manager: LDS -> RDS -> CDS -> EDS cascade.
//!
//! The [`XdsResourceManager`] bridges the xDS client (ADS protocol layer) to the
//! [`XdsCache`](super::cache::XdsCache). It watches resources via
//! [`XdsClient::watch()`] and writes validated resources into the cache for
//! downstream consumers (routing layer, endpoint manager).
//!
//! # Cascade
//!
//! ```text
//! LDS -> RDS (or inline) -> CDS (per cluster) -> EDS (per cluster)
//! ```
//!
//! Each level determines the subscriptions for the next. When the set of
//! referenced clusters changes, the manager reconciles CDS/EDS watches:
//! adding watches for new clusters and dropping watches (+ cache entries)
//! for removed ones.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xds_client::{ResourceEvent, ResourceWatcher, XdsClient};

use crate::common::async_util::AbortOnDrop;
use crate::xds::cache::XdsCache;
use crate::xds::resource::listener::RouteSource;
use crate::xds::resource::{
    ClusterResource, EndpointsResource, ListenerResource, RouteConfigResource,
};

/// Manages the LDS -> RDS -> CDS -> EDS cascade.
///
/// Subscribes to xDS resources via [`XdsClient::watch()`] and writes validated
/// resources into [`XdsCache`]. Dropping the manager aborts all background tasks.
pub(crate) struct XdsResourceManager {
    _lds_task: AbortOnDrop,
}

impl XdsResourceManager {
    /// Creates a new resource manager and starts the cascade.
    ///
    /// # Arguments
    /// * `xds_client` - The xDS client for creating resource watches
    /// * `cache` - The shared cache to write resources into
    /// * `listener_name` - The LDS resource name to watch (from target URI)
    pub(crate) fn new(xds_client: XdsClient, cache: Arc<XdsCache>, listener_name: String) -> Self {
        let state = ListenerWatchState::new();
        let handle = tokio::spawn(state.run(xds_client, cache, listener_name));
        Self {
            _lds_task: AbortOnDrop(handle),
        }
    }
}

/// Mutable state for a single LDS watch and its downstream RDS/CDS/EDS watches.
struct ListenerWatchState {
    /// Active RDS watcher — `None` if the listener uses inline routes.
    rds_watcher: Option<ResourceWatcher<RouteConfigResource>>,
    /// Active RDS name to detect changes across LDS updates.
    rds_name: Option<String>,
    /// Per-cluster CDS+EDS watcher tasks, keyed by cluster name.
    cluster_watches: HashMap<String, ClusterWatchState>,
}

impl ListenerWatchState {
    fn new() -> Self {
        Self {
            rds_watcher: None,
            rds_name: None,
            cluster_watches: HashMap::new(),
        }
    }

    /// Runs the LDS watch and manages the downstream RDS/CDS/EDS cascade.
    async fn run(mut self, xds_client: XdsClient, cache: Arc<XdsCache>, listener_name: String) {
        let mut lds_watcher = xds_client.watch::<ListenerResource>(&listener_name).await;

        loop {
            tokio::select! {
                lds_event = lds_watcher.next() => {
                    // None means xds-client shut down; exit the cascade.
                    let Some(event) = lds_event else { break };
                    self.handle_lds(event, &xds_client, &cache).await;
                }

                rds_event = async {
                    match self.rds_watcher.as_mut() {
                        Some(w) => w.next().await,
                        // No active RDS watch (inline routes); disable this arm.
                        None => std::future::pending().await,
                    }
                } => {
                    // None means the RDS watcher closed; reset and wait for next LDS update.
                    let Some(event) = rds_event else {
                        self.rds_watcher = None;
                        self.rds_name = None;
                        continue;
                    };
                    self.handle_rds(event, &xds_client, &cache).await;
                }
            }
        }
    }

    async fn handle_lds(
        &mut self,
        event: ResourceEvent<ListenerResource>,
        xds_client: &XdsClient,
        cache: &Arc<XdsCache>,
    ) {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(listener),
                done,
            } => {
                match &listener.route_source {
                    RouteSource::Inline(rc) => {
                        // Drop any existing RDS watcher — routes are inline.
                        self.rds_watcher = None;
                        self.rds_name = None;

                        let rc = Arc::new(rc.clone());
                        cache.update_route_config(Arc::clone(&rc));
                        self.reconcile_clusters(&rc, xds_client, cache).await;
                    }
                    RouteSource::Rds(rds_name) => {
                        if self.rds_name.as_deref() != Some(rds_name) {
                            self.rds_watcher =
                                Some(xds_client.watch::<RouteConfigResource>(rds_name).await);
                            self.rds_name = Some(rds_name.clone());
                        }
                    }
                }
                // Cascading watches registered above; dropping signals the xds-client to ACK.
                drop(done);
            }
            // Per gRFC A88: data errors (NACK, resource deletion) with a previously
            // cached resource are treated as ambient — keep using the cached resource
            // to avoid unnecessary outages. Downstream layers (routing, LB) retain
            // their own snapshots independently.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }

    async fn handle_rds(
        &mut self,
        event: ResourceEvent<RouteConfigResource>,
        xds_client: &XdsClient,
        cache: &Arc<XdsCache>,
    ) {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(rc),
                done,
            } => {
                cache.update_route_config(Arc::clone(&rc));
                self.reconcile_clusters(&rc, xds_client, cache).await;
                drop(done);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }

    /// Diffs the current cluster set against the route config's cluster names
    /// and starts/stops per-cluster watcher tasks accordingly.
    async fn reconcile_clusters(
        &mut self,
        route_config: &RouteConfigResource,
        xds_client: &XdsClient,
        cache: &Arc<XdsCache>,
    ) {
        let new_clusters = route_config.cluster_names();
        let old_clusters: HashSet<String> = self.cluster_watches.keys().cloned().collect();

        for name in old_clusters.difference(&new_clusters) {
            self.cluster_watches.remove(name);
            cache.remove_cluster(name);
            cache.remove_endpoints(name);
        }

        for name in new_clusters.difference(&old_clusters) {
            let state = ClusterWatchState::start(name.clone(), xds_client, Arc::clone(cache)).await;
            self.cluster_watches.insert(name.clone(), state);
        }
    }
}

/// State for a per-cluster watcher task.
///
/// Dropping this aborts the CDS watch task, which in turn aborts its child EDS task.
struct ClusterWatchState {
    _cds_task: AbortOnDrop,
}

impl ClusterWatchState {
    /// Spawns a task that manages CDS and EDS watches for a single cluster.
    async fn start(cluster_name: String, xds_client: &XdsClient, cache: Arc<XdsCache>) -> Self {
        let cds_watcher = xds_client.watch::<ClusterResource>(&cluster_name).await;
        let xds_client = xds_client.clone();

        let handle = tokio::spawn(run_cluster_watch(
            cluster_name,
            cds_watcher,
            xds_client,
            cache,
        ));

        Self {
            _cds_task: AbortOnDrop(handle),
        }
    }
}

/// Runs CDS watch for a single cluster and manages its child EDS watch.
///
/// Restarts the EDS watch if the cluster's EDS service name changes.
// `_eds_task` is held for its AbortOnDrop side effect — assignments are intentional.
#[allow(unused_assignments)]
async fn run_cluster_watch(
    cluster_name: String,
    mut cds_watcher: ResourceWatcher<ClusterResource>,
    xds_client: XdsClient,
    cache: Arc<XdsCache>,
) {
    let mut current_eds_name: Option<String> = None;
    let mut _eds_task: Option<AbortOnDrop> = None;

    while let Some(event) = cds_watcher.next().await {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(cluster),
                done,
            } => {
                cache.update_cluster(&cluster_name, Arc::clone(&cluster));

                let eds_name = cluster.eds_service_name().to_string();

                if current_eds_name.as_deref() != Some(&eds_name) {
                    _eds_task = None;

                    let eds_watcher = xds_client.watch::<EndpointsResource>(&eds_name).await;
                    let handle = tokio::spawn(run_eds_watch(
                        cluster_name.clone(),
                        eds_watcher,
                        Arc::clone(&cache),
                    ));
                    _eds_task = Some(AbortOnDrop(handle));
                    current_eds_name = Some(eds_name);
                }

                drop(done);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }
}

/// Runs EDS watch for a single cluster, writing endpoint snapshots to the cache.
async fn run_eds_watch(
    cluster_name: String,
    mut eds_watcher: ResourceWatcher<EndpointsResource>,
    cache: Arc<XdsCache>,
) {
    while let Some(event) = eds_watcher.next().await {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(endpoints),
                ..
            } => {
                cache.update_endpoints(&cluster_name, endpoints);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::route_config::{
        PathSpecifierConfig, RouteConfig, RouteConfigAction, RouteConfigMatch, VirtualHostConfig,
    };
    use xds_client::ProcessingDone;

    fn test_client() -> XdsClient {
        XdsClient::disconnected()
    }

    fn test_cache() -> Arc<XdsCache> {
        Arc::new(XdsCache::new())
    }

    fn make_route_config(name: &str, clusters: &[&str]) -> Arc<RouteConfigResource> {
        Arc::new(RouteConfigResource {
            name: name.into(),
            virtual_hosts: vec![VirtualHostConfig {
                name: "vh".into(),
                domains: vec!["*".into()],
                routes: clusters
                    .iter()
                    .map(|c| RouteConfig {
                        match_criteria: RouteConfigMatch {
                            path_specifier: PathSpecifierConfig::Prefix("/".into()),
                            headers: vec![],
                            case_sensitive: true,
                            match_fraction: None,
                        },
                        action: RouteConfigAction::Cluster((*c).into()),
                    })
                    .collect(),
            }],
        })
    }

    fn make_listener_inline(clusters: &[&str]) -> Arc<ListenerResource> {
        Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Inline(RouteConfigResource {
                name: "inline-rc".into(),
                virtual_hosts: vec![VirtualHostConfig {
                    name: "vh".into(),
                    domains: vec!["*".into()],
                    routes: clusters
                        .iter()
                        .map(|c| RouteConfig {
                            match_criteria: RouteConfigMatch {
                                path_specifier: PathSpecifierConfig::Prefix("/".into()),
                                headers: vec![],
                                case_sensitive: true,
                                match_fraction: None,
                            },
                            action: RouteConfigAction::Cluster((*c).into()),
                        })
                        .collect(),
                }],
            }),
        })
    }

    fn make_listener_rds(rds_name: &str) -> Arc<ListenerResource> {
        Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Rds(rds_name.into()),
        })
    }

    fn ok_event<T>(resource: Arc<T>) -> ResourceEvent<T> {
        ResourceEvent::ResourceChanged {
            result: Ok(resource),
            done: ProcessingDone::noop(),
        }
    }

    fn err_event<T>() -> ResourceEvent<T> {
        ResourceEvent::ResourceChanged {
            result: Err(xds_client::Error::ResourceDoesNotExist),
            done: ProcessingDone::noop(),
        }
    }

    fn ambient_event<T>() -> ResourceEvent<T> {
        ResourceEvent::AmbientError {
            error: xds_client::Error::ResourceDoesNotExist,
            done: ProcessingDone::noop(),
        }
    }

    #[tokio::test]
    async fn reconcile_adds_new_clusters() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc = make_route_config("rc", &["a", "b"]);
        state.reconcile_clusters(&rc, &client, &cache).await;

        assert!(state.cluster_watches.contains_key("a"));
        assert!(state.cluster_watches.contains_key("b"));
        assert_eq!(state.cluster_watches.len(), 2);
    }

    #[tokio::test]
    async fn reconcile_removes_old_clusters() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc1 = make_route_config("rc", &["a", "b"]);
        state.reconcile_clusters(&rc1, &client, &cache).await;

        let rc2 = make_route_config("rc", &["b", "c"]);
        state.reconcile_clusters(&rc2, &client, &cache).await;

        assert!(!state.cluster_watches.contains_key("a"));
        assert!(state.cluster_watches.contains_key("b"));
        assert!(state.cluster_watches.contains_key("c"));
        assert_eq!(state.cluster_watches.len(), 2);
    }

    #[tokio::test]
    async fn reconcile_to_empty_removes_all() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc1 = make_route_config("rc", &["a"]);
        state.reconcile_clusters(&rc1, &client, &cache).await;
        assert_eq!(state.cluster_watches.len(), 1);

        let rc2 = make_route_config("rc", &[]);
        state.reconcile_clusters(&rc2, &client, &cache).await;
        assert!(state.cluster_watches.is_empty());
    }

    #[tokio::test]
    async fn handle_rds_ok_updates_cache_and_reconciles() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc = make_route_config("rc-1", &["cluster-a", "cluster-b"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;

        let config = cache.watch_route_config().next().await.unwrap();
        assert_eq!(config.name, "rc-1");
        assert!(state.cluster_watches.contains_key("cluster-a"));
        assert!(state.cluster_watches.contains_key("cluster-b"));
    }

    #[tokio::test]
    async fn handle_rds_err_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc = make_route_config("rc", &["c1"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;
        assert_eq!(state.cluster_watches.len(), 1);

        // Per gRFC A88: data errors preserve cached state.
        state.handle_rds(err_event(), &client, &cache).await;
        assert_eq!(state.cluster_watches.len(), 1);
    }

    #[tokio::test]
    async fn handle_rds_ambient_error_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        let rc = make_route_config("rc", &["c1"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;
        assert_eq!(state.cluster_watches.len(), 1);

        state.handle_rds(ambient_event(), &client, &cache).await;
        assert_eq!(state.cluster_watches.len(), 1);
    }

    #[tokio::test]
    async fn handle_lds_inline_writes_route_config() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_inline(&["c1"])), &client, &cache)
            .await;

        let config = cache.watch_route_config().next().await.unwrap();
        assert_eq!(config.name, "inline-rc");
        assert!(state.rds_watcher.is_none());
        assert!(state.rds_name.is_none());
        assert!(state.cluster_watches.contains_key("c1"));
    }

    #[tokio::test]
    async fn handle_lds_inline_clears_existing_rds() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_rds("rc-1")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("rc-1"));

        state
            .handle_lds(ok_event(make_listener_inline(&[])), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_none());
        assert!(state.rds_name.is_none());
    }

    #[tokio::test]
    async fn handle_lds_rds_sets_watcher_and_name() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_rds("my-route")), &client, &cache)
            .await;

        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("my-route"));
    }

    #[tokio::test]
    async fn handle_lds_rds_same_name_reuses_watcher() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());

        // Same name — watcher should not be replaced.
        // (We can't check identity, but rds_name should stay the same.)
        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert_eq!(state.rds_name.as_deref(), Some("rc"));
    }

    #[tokio::test]
    async fn handle_lds_rds_different_name_replaces_watcher() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_rds("rc-1")), &client, &cache)
            .await;
        assert_eq!(state.rds_name.as_deref(), Some("rc-1"));

        state
            .handle_lds(ok_event(make_listener_rds("rc-2")), &client, &cache)
            .await;
        assert_eq!(state.rds_name.as_deref(), Some("rc-2"));
    }

    #[tokio::test]
    async fn handle_lds_err_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_inline(&["c1"])), &client, &cache)
            .await;
        assert!(state.cluster_watches.contains_key("c1"));

        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());

        // Per gRFC A88: data errors preserve cached state.
        state.handle_lds(err_event(), &client, &cache).await;
        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("rc"));
        assert!(state.cluster_watches.contains_key("c1"));
    }

    #[tokio::test]
    async fn handle_lds_ambient_error_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = ListenerWatchState::new();

        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());

        state.handle_lds(ambient_event(), &client, &cache).await;
        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("rc"));
    }
}
