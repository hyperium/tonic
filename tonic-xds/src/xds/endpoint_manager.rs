//! Converts snapshot-based endpoint cache updates into incremental
//! [`Change`] streams for Tower's load balancing infrastructure.
//!
//! The resource manager writes [`EndpointsResource`] snapshots into the
//! [`XdsCache`]; this module diffs consecutive snapshots and produces
//! `Change::Insert` / `Change::Remove` events that Tower's P2C balancer
//! (or any other `Discover`-based balancer) can consume.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tower::discover::Change;

use crate::client::endpoint::EndpointAddress;
use crate::xds::cache::CacheWatch;
use crate::xds::resource::EndpointsResource;
use crate::xds::xds_manager::BoxDiscover;

/// Converts endpoint cache watches into incremental [`Change`] streams.
///
/// `EndpointManager` is a pure diff-and-connect component: the caller
/// (typically `XdsResourceManager`) obtains a [`CacheWatch`] from the
/// [`XdsCache`](crate::xds::cache::XdsCache) and passes it here.
pub(crate) struct EndpointManager<S: Send + 'static> {
    /// Creates a service for each new endpoint address (e.g., wrapping a
    /// lazily-connected `tonic::transport::Channel` in an `EndpointChannel`).
    connector: Arc<dyn Fn(&EndpointAddress) -> S + Send + Sync>,
}

impl<S: Send + 'static> EndpointManager<S> {
    pub(crate) fn new(connector: Arc<dyn Fn(&EndpointAddress) -> S + Send + Sync>) -> Self {
        Self { connector }
    }

    /// Returns a stream of endpoint changes for the given cache watch.
    ///
    /// Diffs each snapshot against the previous set of healthy endpoints,
    /// emitting `Change::Insert` for new endpoints and `Change::Remove`
    /// for removed ones.
    pub(crate) fn discover_endpoints(
        &self,
        watch: CacheWatch<EndpointsResource>,
    ) -> BoxDiscover<EndpointAddress, S> {
        let connector = self.connector.clone();
        let (tx, rx) = mpsc::channel(64);

        // The spawned task exits naturally when either:
        // - The CacheWatch closes (cache.remove_endpoints() drops the watch sender)
        // - The receiver is dropped (consumer no longer reading Change events)
        tokio::spawn(diff_loop(watch, connector, tx));

        Box::pin(ReceiverStream::new(rx))
    }
}

/// Background task: watches endpoint snapshots and emits incremental changes.
///
/// Each time a new [`EndpointsResource`] arrives from the cache, we diff
/// `healthy_endpoints()` against the previous set and emit `Remove` for
/// gone endpoints followed by `Insert` for new ones.
async fn diff_loop<S: Send + 'static>(
    mut watch: CacheWatch<EndpointsResource>,
    connector: Arc<dyn Fn(&EndpointAddress) -> S + Send + Sync>,
    tx: mpsc::Sender<Result<Change<EndpointAddress, S>, BoxError>>,
) {
    let mut active: HashSet<EndpointAddress> = HashSet::new();

    while let Some(endpoints) = watch.next().await {
        let new_set: HashSet<EndpointAddress> = endpoints
            .healthy_endpoints()
            .map(|ep| ep.address.clone())
            .collect();

        for removed in active.difference(&new_set) {
            if tx.send(Ok(Change::Remove(removed.clone()))).await.is_err() {
                return;
            }
        }

        for added in new_set.difference(&active) {
            let svc = connector(added);
            if tx
                .send(Ok(Change::Insert(added.clone(), svc)))
                .await
                .is_err()
            {
                return;
            }
        }

        active = new_set;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::cache::XdsCache;
    use crate::xds::resource::endpoints::{HealthStatus, LocalityEndpoints, ResolvedEndpoint};
    use tokio_stream::StreamExt;

    fn test_connector() -> Arc<dyn Fn(&EndpointAddress) -> String + Send + Sync> {
        Arc::new(|addr: &EndpointAddress| addr.to_string())
    }

    fn make_endpoints(cluster: &str, addrs: &[(&str, u16)]) -> Arc<EndpointsResource> {
        Arc::new(EndpointsResource {
            cluster_name: cluster.to_string(),
            localities: vec![LocalityEndpoints {
                locality: None,
                endpoints: addrs
                    .iter()
                    .map(|(host, port)| ResolvedEndpoint {
                        address: EndpointAddress::new(*host, *port),
                        health_status: HealthStatus::Healthy,
                        load_balancing_weight: 1,
                    })
                    .collect(),
                load_balancing_weight: 100,
                priority: 0,
            }],
        })
    }

    #[tokio::test]
    async fn initial_endpoints_emitted_as_inserts() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints(
            "c1",
            make_endpoints("c1", &[("10.0.0.1", 8080), ("10.0.0.2", 8080)]),
        );

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));

        let mut addrs: Vec<String> = Vec::new();
        for _ in 0..2 {
            match stream.next().await.unwrap().unwrap() {
                Change::Insert(addr, _svc) => addrs.push(addr.to_string()),
                Change::Remove(_) => panic!("expected Insert"),
            }
        }
        addrs.sort();
        assert_eq!(addrs, vec!["10.0.0.1:8080", "10.0.0.2:8080"]);
    }

    #[tokio::test]
    async fn added_endpoint_emits_insert() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        let _ = stream.next().await; // consume initial

        cache.update_endpoints(
            "c1",
            make_endpoints("c1", &[("10.0.0.1", 8080), ("10.0.0.2", 8080)]),
        );

        match stream.next().await.unwrap().unwrap() {
            Change::Insert(addr, _) => assert_eq!(addr.to_string(), "10.0.0.2:8080"),
            Change::Remove(_) => panic!("expected Insert for new endpoint"),
        }
    }

    #[tokio::test]
    async fn removed_endpoint_emits_remove() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints(
            "c1",
            make_endpoints("c1", &[("10.0.0.1", 8080), ("10.0.0.2", 8080)]),
        );

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        // Consume 2 initial inserts.
        let _ = stream.next().await;
        let _ = stream.next().await;

        // Shrink to one endpoint.
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        match stream.next().await.unwrap().unwrap() {
            Change::Remove(addr) => assert_eq!(addr.to_string(), "10.0.0.2:8080"),
            Change::Insert(..) => panic!("expected Remove"),
        }
    }

    #[tokio::test]
    async fn unhealthy_endpoint_removed() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        let _ = stream.next().await; // consume initial insert

        let unhealthy = Arc::new(EndpointsResource {
            cluster_name: "c1".to_string(),
            localities: vec![LocalityEndpoints {
                locality: None,
                endpoints: vec![ResolvedEndpoint {
                    address: EndpointAddress::new("10.0.0.1", 8080),
                    health_status: HealthStatus::Unhealthy,
                    load_balancing_weight: 1,
                }],
                load_balancing_weight: 100,
                priority: 0,
            }],
        });
        cache.update_endpoints("c1", unhealthy);

        match stream.next().await.unwrap().unwrap() {
            Change::Remove(addr) => assert_eq!(addr.to_string(), "10.0.0.1:8080"),
            Change::Insert(..) => panic!("expected Remove for unhealthy endpoint"),
        }
    }

    #[tokio::test]
    async fn cache_removal_closes_stream() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        let _ = stream.next().await; // consume initial

        cache.remove_endpoints("c1");

        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn multiple_clusters_independent() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));
        cache.update_endpoints("c2", make_endpoints("c2", &[("10.0.0.2", 9090)]));

        let mut s1 = manager.discover_endpoints(cache.watch_endpoints("c1"));
        let mut s2 = manager.discover_endpoints(cache.watch_endpoints("c2"));

        match s1.next().await.unwrap().unwrap() {
            Change::Insert(addr, _) => assert_eq!(addr.to_string(), "10.0.0.1:8080"),
            _ => panic!("expected Insert"),
        }
        match s2.next().await.unwrap().unwrap() {
            Change::Insert(addr, _) => assert_eq!(addr.to_string(), "10.0.0.2:9090"),
            _ => panic!("expected Insert"),
        }
    }

    #[tokio::test]
    async fn endpoint_swap_emits_remove_then_insert() {
        let cache = XdsCache::new();
        let manager = EndpointManager::new(test_connector());

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        let _ = stream.next().await; // consume initial

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.2", 8080)]));

        let mut saw_remove = false;
        let mut saw_insert = false;
        for _ in 0..2 {
            match stream.next().await.unwrap().unwrap() {
                Change::Remove(addr) => {
                    assert_eq!(addr.to_string(), "10.0.0.1:8080");
                    saw_remove = true;
                }
                Change::Insert(addr, _) => {
                    assert_eq!(addr.to_string(), "10.0.0.2:8080");
                    saw_insert = true;
                }
            }
        }
        assert!(saw_remove, "should have removed old endpoint");
        assert!(saw_insert, "should have inserted new endpoint");
    }
}
