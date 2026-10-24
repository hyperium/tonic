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

//! xDS resource manager: LDS -> RDS -> CDS -> EDS cascade.
//!
//! The [`XdsResourceManager`] bridges the xDS client (ADS protocol layer) to the
//! [`XdsCache`]. It watches resources via
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
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tokio_stream::{StreamExt, StreamMap};
use xds_client::{Resource, ResourceEvent, ResourceWatcher, XdsClient};

use crate::common::async_util::AbortOnDrop;
use crate::xds::cache::XdsCache;
use crate::xds::resource::listener::RouteSource;
use crate::xds::resource::{
    ClusterResource, EndpointsResource, ListenerResource, RouteConfigResource,
};

/// Adapter to use [`ResourceWatcher`] with [`StreamMap`].
struct WatcherStream<T: Resource>(ResourceWatcher<T>);

impl<T: Resource> Unpin for WatcherStream<T> {}

impl<T: Resource> Stream for WatcherStream<T> {
    type Item = ResourceEvent<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().0.poll_next(cx)
    }
}

/// Manages the LDS -> RDS -> CDS -> EDS cascade.
///
/// Subscribes to xDS resources via [`XdsClient::watch()`] and writes validated
/// resources into [`XdsCache`]. Dropping the manager aborts the background task.
pub(crate) struct XdsResourceManager {
    _task: AbortOnDrop,
}

impl XdsResourceManager {
    /// Creates a new resource manager and starts the cascade.
    ///
    /// # Arguments
    /// * `xds_client` - The xDS client for creating resource watches
    /// * `cache` - The shared cache to write resources into
    /// * `listener_name` - The LDS resource name to watch (from target URI)
    pub(crate) fn new(xds_client: XdsClient, cache: Arc<XdsCache>, listener_name: String) -> Self {
        let state = CascadeState::new();
        let handle = tokio::spawn(state.run(xds_client, cache, listener_name));
        Self {
            _task: AbortOnDrop(handle),
        }
    }
}

/// Mutable state for the entire LDS -> RDS -> CDS -> EDS cascade.
///
/// All four resource levels are polled in a single task via [`run`](Self::run).
struct CascadeState {
    /// Active RDS watcher — `None` if the listener uses inline routes.
    rds_watcher: Option<ResourceWatcher<RouteConfigResource>>,
    /// Active RDS name to detect changes across LDS updates.
    rds_name: Option<String>,
    /// Latest route config received for `rds_name`, as received (without the
    /// listener's defaults), so a listener-only change can be republished.
    rds_route_config: Option<Arc<RouteConfigResource>>,
    /// The current listener's HCM default max stream duration (gRFC A31),
    /// applied to routes that set none when a route config is published.
    listener_max_stream_duration: Option<Duration>,
    /// Per-cluster CDS watchers, keyed by cluster name.
    cds_watchers: StreamMap<String, WatcherStream<ClusterResource>>,
    /// Per-cluster EDS watchers, keyed by cluster name.
    eds_watchers: StreamMap<String, WatcherStream<EndpointsResource>>,
    /// Current EDS service name per cluster, to detect when the name changes.
    eds_names: HashMap<String, String>,
}

impl CascadeState {
    fn new() -> Self {
        Self {
            rds_watcher: None,
            rds_name: None,
            rds_route_config: None,
            listener_max_stream_duration: None,
            cds_watchers: StreamMap::new(),
            eds_watchers: StreamMap::new(),
            eds_names: HashMap::new(),
        }
    }

    /// Runs the cascade select loop. All resource events are processed here.
    ///
    /// Biased so higher-level events (LDS/RDS) are processed before
    /// lower-level ones (CDS/EDS), avoiding wasted work on clusters
    /// about to be removed.
    async fn run(mut self, xds_client: XdsClient, cache: Arc<XdsCache>, listener_name: String) {
        let mut lds_watcher = xds_client.watch::<ListenerResource>(&listener_name).await;

        loop {
            tokio::select! {
                biased;

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
                        self.rds_route_config = None;
                        continue;
                    };
                    self.handle_rds(event, &xds_client, &cache).await;
                }

                Some((name, event)) = self.cds_watchers.next(),
                    if !self.cds_watchers.is_empty() =>
                {
                    self.handle_cds(&name, event, &xds_client, &cache).await;
                }

                Some((name, event)) = self.eds_watchers.next(),
                    if !self.eds_watchers.is_empty() =>
                {
                    self.handle_eds(&name, event, &cache);
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
                let listener_msd_changed =
                    self.listener_max_stream_duration != listener.max_stream_duration;
                self.listener_max_stream_duration = listener.max_stream_duration;
                match &listener.route_source {
                    RouteSource::Inline(rc) => {
                        // Drop any existing RDS watcher — routes are inline.
                        self.rds_watcher = None;
                        self.rds_name = None;
                        self.rds_route_config = None;

                        let rc = Arc::new(rc.clone());
                        self.publish_route_config(&rc, cache);
                        self.reconcile_clusters(&rc, xds_client, cache).await;
                    }
                    RouteSource::Rds(rds_name) => {
                        if self.rds_name.as_deref() != Some(rds_name) {
                            // Keep serving the previous route config until the
                            // new one arrives; it is published from `handle_rds`.
                            self.rds_watcher =
                                Some(xds_client.watch::<RouteConfigResource>(rds_name).await);
                            self.rds_name = Some(rds_name.clone());
                            self.rds_route_config = None;
                        } else if listener_msd_changed && let Some(rc) = &self.rds_route_config {
                            // Same route config, new listener defaults.
                            self.publish_route_config(rc, cache);
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
                self.publish_route_config(&rc, cache);
                self.rds_route_config = Some(Arc::clone(&rc));
                self.reconcile_clusters(&rc, xds_client, cache).await;
                drop(done);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }

    /// Publishes `rc` with the listener's default max stream duration applied
    /// to routes that set none (gRFC A31), so routing reads a single per-route
    /// value.
    fn publish_route_config(&self, rc: &Arc<RouteConfigResource>, cache: &XdsCache) {
        let rc = match self.listener_max_stream_duration {
            // Zero means no limit, the same as unset, so there is nothing to apply.
            Some(default) if !default.is_zero() => {
                Arc::new((**rc).clone().with_default_max_stream_duration(default))
            }
            _ => Arc::clone(rc),
        };
        cache.update_route_config(rc);
    }

    async fn handle_cds(
        &mut self,
        cluster_name: &str,
        event: ResourceEvent<ClusterResource>,
        xds_client: &XdsClient,
        cache: &Arc<XdsCache>,
    ) {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(cluster),
                done,
            } => {
                cache.update_cluster(cluster_name, Arc::clone(&cluster));

                let eds_name = cluster.eds_service_name().to_string();
                if self.eds_names.get(cluster_name).map(|s| s.as_str()) != Some(&eds_name) {
                    self.eds_watchers.remove(cluster_name);

                    let watcher = xds_client.watch::<EndpointsResource>(&eds_name).await;
                    let cluster_key = cluster_name.to_string();
                    self.eds_watchers
                        .insert(cluster_key.clone(), WatcherStream(watcher));
                    self.eds_names.insert(cluster_key, eds_name);
                }
                drop(done);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }

    fn handle_eds(
        &self,
        cluster_name: &str,
        event: ResourceEvent<EndpointsResource>,
        cache: &Arc<XdsCache>,
    ) {
        match event {
            ResourceEvent::ResourceChanged {
                result: Ok(endpoints),
                ..
            } => {
                cache.update_endpoints(cluster_name, endpoints);
            }
            // Per gRFC A88: keep using cached resources on data errors.
            ResourceEvent::ResourceChanged { result: Err(_), .. }
            | ResourceEvent::AmbientError { .. } => {}
        }
    }

    /// Diffs the current cluster set against the route config's cluster names
    /// and starts/stops per-cluster watchers accordingly.
    async fn reconcile_clusters(
        &mut self,
        route_config: &RouteConfigResource,
        xds_client: &XdsClient,
        cache: &Arc<XdsCache>,
    ) {
        let new_clusters = route_config.cluster_names();
        let old_clusters: HashSet<String> = self.cds_watchers.keys().cloned().collect();

        for name in old_clusters.difference(&new_clusters) {
            self.cds_watchers.remove(name);
            self.eds_watchers.remove(name);
            self.eds_names.remove(name);
            cache.remove_cluster(name);
            cache.remove_endpoints(name);
        }

        for name in new_clusters.difference(&old_clusters) {
            let watcher = xds_client.watch::<ClusterResource>(name).await;
            self.cds_watchers
                .insert(name.clone(), WatcherStream(watcher));
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
                        retry_config: None,
                        max_stream_duration: None,
                    })
                    .collect(),
            }],
            metadata: Default::default(),
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
                            retry_config: None,
                            max_stream_duration: None,
                        })
                        .collect(),
                }],
                metadata: Default::default(),
            }),
            max_stream_duration: None,
        })
    }

    fn make_listener_rds(rds_name: &str) -> Arc<ListenerResource> {
        make_listener_rds_with_msd(rds_name, None)
    }

    fn make_listener_rds_with_msd(
        rds_name: &str,
        max_stream_duration: Option<Duration>,
    ) -> Arc<ListenerResource> {
        Arc::new(ListenerResource {
            name: "listener".into(),
            route_source: RouteSource::Rds(rds_name.into()),
            max_stream_duration,
        })
    }

    fn ok_event<T>(resource: Arc<T>) -> ResourceEvent<T> {
        ResourceEvent::ResourceChanged {
            result: Ok(resource),
            done: ProcessingDone::detached(),
        }
    }

    fn err_event<T>() -> ResourceEvent<T> {
        ResourceEvent::ResourceChanged {
            result: Err(xds_client::Error::ResourceDoesNotExist),
            done: ProcessingDone::detached(),
        }
    }

    fn ambient_event<T>() -> ResourceEvent<T> {
        ResourceEvent::AmbientError {
            error: xds_client::Error::ResourceDoesNotExist,
            done: ProcessingDone::detached(),
        }
    }

    #[tokio::test]
    async fn reconcile_adds_new_clusters() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc = make_route_config("rc", &["a", "b"]);
        state.reconcile_clusters(&rc, &client, &cache).await;

        assert!(state.cds_watchers.contains_key("a"));
        assert!(state.cds_watchers.contains_key("b"));
        assert_eq!(state.cds_watchers.keys().count(), 2);
    }

    #[tokio::test]
    async fn reconcile_removes_old_clusters() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc1 = make_route_config("rc", &["a", "b"]);
        state.reconcile_clusters(&rc1, &client, &cache).await;

        let rc2 = make_route_config("rc", &["b", "c"]);
        state.reconcile_clusters(&rc2, &client, &cache).await;

        assert!(!state.cds_watchers.contains_key("a"));
        assert!(state.cds_watchers.contains_key("b"));
        assert!(state.cds_watchers.contains_key("c"));
        assert_eq!(state.cds_watchers.keys().count(), 2);
    }

    #[tokio::test]
    async fn reconcile_to_empty_removes_all() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc1 = make_route_config("rc", &["a"]);
        state.reconcile_clusters(&rc1, &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 1);

        let rc2 = make_route_config("rc", &[]);
        state.reconcile_clusters(&rc2, &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 0);
    }

    #[tokio::test]
    async fn handle_rds_ok_updates_cache_and_reconciles() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc = make_route_config("rc-1", &["cluster-a", "cluster-b"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;

        let config = cache.watch_route_config().next().await.unwrap();
        assert_eq!(config.name, "rc-1");
        assert!(state.cds_watchers.contains_key("cluster-a"));
        assert!(state.cds_watchers.contains_key("cluster-b"));
    }

    #[tokio::test]
    async fn handle_rds_err_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc = make_route_config("rc", &["c1"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 1);

        // Per gRFC A88: data errors preserve cached state.
        state.handle_rds(err_event(), &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 1);
    }

    #[tokio::test]
    async fn handle_rds_ambient_error_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let rc = make_route_config("rc", &["c1"]);
        state.handle_rds(ok_event(rc), &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 1);

        state.handle_rds(ambient_event(), &client, &cache).await;
        assert_eq!(state.cds_watchers.keys().count(), 1);
    }

    #[tokio::test]
    async fn handle_lds_inline_writes_route_config() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        state
            .handle_lds(ok_event(make_listener_inline(&["c1"])), &client, &cache)
            .await;

        let config = cache.watch_route_config().next().await.unwrap();
        assert_eq!(config.name, "inline-rc");
        assert!(state.rds_watcher.is_none());
        assert!(state.rds_name.is_none());
        assert!(state.cds_watchers.contains_key("c1"));
    }

    #[tokio::test]
    async fn handle_lds_inline_clears_existing_rds() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

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
        let mut state = CascadeState::new();

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
        let mut state = CascadeState::new();

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
        let mut state = CascadeState::new();

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
        let mut state = CascadeState::new();

        state
            .handle_lds(ok_event(make_listener_inline(&["c1"])), &client, &cache)
            .await;
        assert!(state.cds_watchers.contains_key("c1"));

        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());

        // Per gRFC A88: data errors preserve cached state.
        state.handle_lds(err_event(), &client, &cache).await;
        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("rc"));
        assert!(state.cds_watchers.contains_key("c1"));
    }

    #[tokio::test]
    async fn handle_lds_ambient_error_preserves_state() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        state
            .handle_lds(ok_event(make_listener_rds("rc")), &client, &cache)
            .await;
        assert!(state.rds_watcher.is_some());

        state.handle_lds(ambient_event(), &client, &cache).await;
        assert!(state.rds_watcher.is_some());
        assert_eq!(state.rds_name.as_deref(), Some("rc"));
    }

    /// The route config currently published by `cache`.
    async fn published(cache: &XdsCache) -> Arc<RouteConfigResource> {
        cache.watch_route_config().next().await.unwrap()
    }

    /// The max stream duration of the published config's first route.
    async fn published_msd(cache: &XdsCache) -> Option<Duration> {
        published(cache).await.virtual_hosts[0].routes[0].max_stream_duration
    }

    #[tokio::test]
    async fn handle_lds_inline_applies_listener_max_stream_duration() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();

        let mut listener = Arc::into_inner(make_listener_inline(&["c1"])).unwrap();
        listener.max_stream_duration = Some(Duration::from_secs(4));
        state
            .handle_lds(ok_event(Arc::new(listener)), &client, &cache)
            .await;

        assert_eq!(published_msd(&cache).await, Some(Duration::from_secs(4)));
    }

    #[tokio::test]
    async fn rds_route_config_follows_listener_max_stream_duration() {
        let cache = test_cache();
        let client = test_client();
        let mut state = CascadeState::new();
        let secs = |s| Some(Duration::from_secs(s));

        // The RDS config is published with the listener's default applied.
        let listener = make_listener_rds_with_msd("rc", secs(7));
        state
            .handle_lds(ok_event(Arc::clone(&listener)), &client, &cache)
            .await;
        let rc = make_route_config("rc", &["c1"]);
        state
            .handle_rds(ok_event(Arc::clone(&rc)), &client, &cache)
            .await;
        let config = published(&cache).await;
        assert_eq!(config.name, "rc");
        assert_eq!(published_msd(&cache).await, secs(7));

        // An unchanged listener does not republish.
        state.handle_lds(ok_event(listener), &client, &cache).await;
        assert!(Arc::ptr_eq(&published(&cache).await, &config));

        // A listener-only change republishes the route config with the new
        // default, applied to the config as received rather than the old one.
        let listener = make_listener_rds_with_msd("rc", secs(3));
        state.handle_lds(ok_event(listener), &client, &cache).await;
        assert_eq!(published_msd(&cache).await, secs(3));
        let listener = make_listener_rds_with_msd("rc", None);
        state.handle_lds(ok_event(listener), &client, &cache).await;
        assert!(Arc::ptr_eq(&published(&cache).await, &rc));

        // A new RDS name keeps serving the old config until the new one arrives,
        // which is then published with the new default.
        let listener = make_listener_rds_with_msd("rc-2", secs(9));
        state.handle_lds(ok_event(listener), &client, &cache).await;
        assert!(Arc::ptr_eq(&published(&cache).await, &rc));

        state
            .handle_rds(
                ok_event(make_route_config("rc-2", &["c1"])),
                &client,
                &cache,
            )
            .await;
        assert_eq!(published(&cache).await.name, "rc-2");
        assert_eq!(published_msd(&cache).await, secs(9));
    }

    /// End-to-end replica of the production Istio hang: an RDS update that
    /// references far more clusters than the xds-client worker's command
    /// channel buffers (64), reconciled through the real cascade path —
    /// `handle_rds` holds the event's `ProcessingDone` token while
    /// `reconcile_clusters` awaits one `watch()` per cluster.
    ///
    /// Before xds-client gained ADS flow control, the worker sat inside
    /// `handle_response` awaiting that token and stopped draining commands;
    /// once the command channel filled, `reconcile_clusters` blocked on the
    /// 65th watch and the client deadlocked. This test times out under that
    /// behavior and completes under the fixed worker.
    #[tokio::test]
    async fn rds_referencing_many_clusters_reconciles_without_deadlock() {
        use xds_client::{
            ClientConfig, Node, ProstCodec, TokioRuntime, TonicTransportBuilder,
            XdsClient as RealXdsClient,
        };
        use xds_test_util::{XdsTestControlPlaneService, config};

        // Well past the worker's 64-slot command channel.
        const CLUSTER_COUNT: usize = 100;

        let control_plane = XdsTestControlPlaneService::new()
            .start()
            .await
            .expect("control plane failed to start");
        control_plane.get_service().set_xds_config(
            &config::AdsTypeUrl::Rds,
            HashMap::from([(
                "rc-many".to_string(),
                config::build_route_config_with_cluster_count("rc-many", CLUSTER_COUNT),
            )]),
        );

        let client_config = ClientConfig::new(
            Node::new("test", "0"),
            format!("http://{}", control_plane.addr()),
        );
        let xds_client: RealXdsClient = RealXdsClient::builder(
            client_config,
            TonicTransportBuilder::new(),
            ProstCodec,
            TokioRuntime,
        )
        .build();

        let mut watcher = xds_client.watch::<RouteConfigResource>("rc-many").await;
        let event = tokio::time::timeout(Duration::from_secs(10), watcher.next())
            .await
            .expect("timed out waiting for the RDS update")
            .expect("watcher closed");

        let cache = test_cache();
        let mut state = CascadeState::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            state.handle_rds(event, &xds_client, &cache),
        )
        .await
        .expect("deadlock: reconcile_clusters blocked while the worker awaited ProcessingDone");

        assert_eq!(state.cds_watchers.len(), CLUSTER_COUNT);
    }
}
