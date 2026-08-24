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

//! Converts snapshot-based endpoint cache updates into incremental
//! [`Change`] streams for Tower's load balancing infrastructure.
//!
//! The resource manager writes [`EndpointsResource`] snapshots into the
//! `XdsCache`; this module diffs consecutive snapshots and produces
//! `Change::Insert` / `Change::Remove` events that Tower's P2C balancer
//! (or any other `Discover`-based balancer) can consume.
//!
//! Two flavours share one diff loop, differing only in what an added endpoint
//! is turned into: [`EndpointManager::discover_endpoints`] connects it through
//! the cluster's [`ClusterConnector`], while [`discover_addresses`] emits a
//! bare [`IdleChannel`] for balancers that own the connection lifecycle
//! themselves.
//!
//! Either way, a snapshot is applied only once the cluster's connector for the
//! same cache generation is published, and removing the endpoints resource
//! removes every endpoint, so a re-added cluster starts from scratch.

use std::collections::HashSet;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use tower::discover::Change;

use crate::client::endpoint::EndpointAddress;
use crate::client::lb::BoxDiscover;
use crate::client::loadbalance::channel_state::IdleChannel;
use crate::common::async_util::BoxFuture;
use crate::xds::cache::{CacheEvent, CacheWatch};
use crate::xds::connector::{ClusterConnector, SharedConnector};
use crate::xds::resource::EndpointsResource;

/// Buffer capacity for the endpoint change channel between the diff loop
/// and Tower's load balancer.
const ENDPOINT_CHANNEL_CAPACITY: usize = 64;

/// Converts endpoint cache watches into incremental [`Change`] streams.
///
/// `EndpointManager` is a pure diff-and-connect component: the caller
/// (typically `XdsClusterDiscovery`) obtains a [`CacheWatch`] from the
/// [`XdsCache`](crate::xds::cache::XdsCache) and passes it here, plus the
/// cluster's [`ClusterConnector`], which supplies the connector for each
/// snapshot's generation.
pub(crate) struct EndpointManager<S: Send + 'static> {
    connector: ClusterConnector<S>,
}

impl<S: Send + 'static> EndpointManager<S> {
    pub(crate) fn new(connector: ClusterConnector<S>) -> Self {
        Self { connector }
    }

    /// Returns a stream of endpoint changes for the given cache watch.
    ///
    /// Diffs each snapshot against the previous set of healthy endpoints,
    /// emitting `Change::Insert` for new endpoints and `Change::Remove`
    /// for removed ones.
    ///
    /// Each inserted endpoint is connected through the cluster's connector, so it
    /// reaches the balancer already established. A CDS update therefore only
    /// reaches endpoints discovered after it: channels already handed out keep
    /// the connection (and any TLS session) they were created with.
    pub(crate) fn discover_endpoints(
        &self,
        watch: CacheWatch<EndpointsResource>,
    ) -> BoxDiscover<EndpointAddress, S> {
        spawn_diff_loop(watch, self.connector.clone(), |connector, addr| {
            connector.connect(addr)
        })
    }
}

/// Returns a stream of endpoint changes carrying addresses only.
///
/// Each added endpoint is emitted as an [`IdleChannel`] — the initial state of
/// the load balancer's channel state machine — without connecting. For
/// balancers that drive connect / eject / reconnect themselves and so need the
/// address rather than a live channel. `connector` only gates each snapshot on
/// its generation's connector being published; connecting is left to the
/// balancer.
pub(crate) fn discover_addresses<C: Send + 'static>(
    watch: CacheWatch<EndpointsResource>,
    connector: ClusterConnector<C>,
) -> BoxDiscover<EndpointAddress, IdleChannel> {
    spawn_diff_loop(watch, connector, |_, addr| {
        let channel = IdleChannel::new(addr.clone());
        Box::pin(async move { channel })
    })
}

/// Spawns the shared diff loop and returns the change stream it feeds.
///
/// The spawned task exits when the cache or the consumer is dropped.
fn spawn_diff_loop<C, S, F>(
    watch: CacheWatch<EndpointsResource>,
    connector: ClusterConnector<C>,
    make_service: F,
) -> BoxDiscover<EndpointAddress, S>
where
    C: Send + 'static,
    S: Send + 'static,
    F: Fn(&SharedConnector<C>, &EndpointAddress) -> BoxFuture<S> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(ENDPOINT_CHANNEL_CAPACITY);
    tokio::spawn(diff_loop(watch, connector, make_service, tx));
    Box::pin(ReceiverStream::new(rx))
}

/// Background task: watches endpoint snapshots and emits incremental changes.
///
/// Each time a new [`EndpointsResource`] arrives from the cache, we diff
/// `healthy_endpoints()` against the endpoints already emitted and emit
/// `Insert` for new endpoints followed by `Remove` for gone ones. A snapshot
/// superseded while it is being applied is abandoned part-way; the next one is
/// diffed against what was actually emitted.
async fn diff_loop<C, S, F>(
    mut watch: CacheWatch<EndpointsResource>,
    connector: ClusterConnector<C>,
    make_service: F,
    tx: mpsc::Sender<Result<Change<EndpointAddress, S>, BoxError>>,
) where
    C: Send + 'static,
    S: Send + 'static,
    F: Fn(&SharedConnector<C>, &EndpointAddress) -> BoxFuture<S>,
{
    let mut active: HashSet<EndpointAddress> = HashSet::new();

    'events: loop {
        let event = tokio::select! {
            _ = tx.closed() => return,
            event = watch.next_event() => event,
        };
        let (generation, revision, endpoints) = match event {
            None => return,
            Some(CacheEvent::Removed { .. }) => {
                for removed in active.drain() {
                    if tx.send(Ok(Change::Remove(removed))).await.is_err() {
                        return;
                    }
                }
                continue;
            }
            Some(CacheEvent::Resource {
                generation,
                revision,
                resource,
            }) => (generation, revision, resource),
        };

        let cluster_connector = tokio::select! {
            _ = tx.closed() => return,
            connector = connector.connector_for_generation(generation) => connector,
        };
        // `None`: the cluster was removed after this snapshot was written, and
        // the endpoints removal that follows supersedes it.
        let Some(cluster_connector) = cluster_connector else {
            continue;
        };
        if watch.current_revision() != revision {
            continue;
        }

        let new_set: HashSet<EndpointAddress> = endpoints
            .healthy_endpoints()
            .map(|ep| ep.address.clone())
            .collect();
        let added: Vec<_> = new_set.difference(&active).cloned().collect();
        let removed: Vec<_> = active.difference(&new_set).cloned().collect();

        for added in added {
            if watch.current_revision() != revision {
                continue 'events;
            }
            let svc = make_service(&cluster_connector, &added).await;
            if watch.current_revision() != revision {
                continue 'events;
            }
            if tx
                .send(Ok(Change::Insert(added.clone(), svc)))
                .await
                .is_err()
            {
                return;
            }
            active.insert(added);
        }

        for removed in removed {
            if watch.current_revision() != revision {
                continue 'events;
            }
            if tx.send(Ok(Change::Remove(removed.clone()))).await.is_err() {
                return;
            }
            active.remove(&removed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::Connector;
    use crate::common::async_util::BoxFuture;
    use crate::xds::cache::XdsCache;
    use crate::xds::connector::ConnectorState;
    use crate::xds::resource::endpoints::{HealthStatus, LocalityEndpoints, ResolvedEndpoint};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch;
    use tokio_stream::StreamExt;

    /// Test [`Connector`] that returns the address as its `Service` (just a
    /// `String`).
    struct StringConnector;

    impl Connector for StringConnector {
        type Service = String;
        fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
            let s = addr.to_string();
            Box::pin(async move { s })
        }
    }

    fn string_connector() -> SharedConnector<String> {
        Arc::new(StringConnector)
    }

    /// A cluster connector holding a connector for `generation`, plus the
    /// sender that publishes later generations in place of a CDS watch.
    fn test_connector_channel(
        generation: u64,
    ) -> (
        watch::Sender<ConnectorState<String>>,
        ClusterConnector<String>,
    ) {
        let (tx, rx) = watch::channel(ConnectorState::new(generation, Some(string_connector())));
        (tx, ClusterConnector::from_receiver("c1", rx))
    }

    fn test_connector() -> ClusterConnector<String> {
        test_connector_channel(0).1
    }

    async fn expect_insert<S>(stream: &mut BoxDiscover<EndpointAddress, S>, expected: &str) {
        match stream.next().await.unwrap().unwrap() {
            Change::Insert(addr, _) => assert_eq!(addr.to_string(), expected),
            Change::Remove(addr) => panic!("expected Insert of {expected}, got Remove of {addr}"),
        }
    }

    async fn expect_remove<S>(stream: &mut BoxDiscover<EndpointAddress, S>, expected: &str) {
        match stream.next().await.unwrap().unwrap() {
            Change::Remove(addr) => assert_eq!(addr.to_string(), expected),
            Change::Insert(addr, _) => {
                panic!("expected Remove of {expected}, got Insert of {addr}")
            }
        }
    }

    async fn expect_idle<S>(stream: &mut BoxDiscover<EndpointAddress, S>) {
        assert!(
            tokio::time::timeout(Duration::from_millis(20), stream.next())
                .await
                .is_err(),
            "expected no endpoint change",
        );
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
    async fn cache_removal_clears_endpoints_and_readd_recovers() {
        let cache = XdsCache::new();
        let (connector_tx, connector) = test_connector_channel(0);
        let manager = EndpointManager::new(connector);

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));

        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        expect_insert(&mut stream, "10.0.0.1:8080").await;

        cache.remove_endpoints("c1");
        expect_remove(&mut stream, "10.0.0.1:8080").await;

        connector_tx.send_replace(ConnectorState::new(1, Some(string_connector())));
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));
        expect_insert(&mut stream, "10.0.0.1:8080").await;
    }

    #[tokio::test]
    async fn readded_endpoints_wait_for_matching_connector_generation() {
        let cache = XdsCache::new();
        let (connector_tx, connector) = test_connector_channel(0);
        let manager = EndpointManager::new(connector);
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));
        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        expect_insert(&mut stream, "10.0.0.1:8080").await;

        cache.remove_endpoints("c1");
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.2", 8080)]));
        expect_remove(&mut stream, "10.0.0.1:8080").await;
        // The connector still belongs to the removed generation.
        expect_idle(&mut stream).await;

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.3", 8080)]));
        connector_tx.send_replace(ConnectorState::new(1, Some(string_connector())));
        expect_insert(&mut stream, "10.0.0.3:8080").await;
    }

    #[tokio::test]
    async fn snapshot_superseded_by_cluster_removal_is_skipped() {
        let cache = XdsCache::new();
        let (connector_tx, connector) = test_connector_channel(0);
        connector_tx.send_replace(ConnectorState::new(0, None));
        let manager = EndpointManager::new(connector);
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));
        let mut stream = manager.discover_endpoints(cache.watch_endpoints("c1"));
        expect_idle(&mut stream).await;

        // CDS removal advances the connector past the pending snapshot, which
        // is dropped rather than connected once the cluster is re-added.
        connector_tx.send_replace(ConnectorState::new(1, None));
        cache.remove_endpoints("c1");
        connector_tx.send_replace(ConnectorState::new(1, Some(string_connector())));
        expect_idle(&mut stream).await;

        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.2", 8080)]));
        expect_insert(&mut stream, "10.0.0.2:8080").await;
    }

    /// The address-only flavour shares the diff loop, so it gets the same
    /// generation gating and removal handling.
    #[tokio::test]
    async fn address_discovery_waits_for_connector_and_clears_on_removal() {
        let cache = XdsCache::new();
        let (connector_tx, connector) = test_connector_channel(0);
        connector_tx.send_replace(ConnectorState::new(0, None));
        cache.update_endpoints("c1", make_endpoints("c1", &[("10.0.0.1", 8080)]));
        let mut stream = discover_addresses(cache.watch_endpoints("c1"), connector);
        expect_idle(&mut stream).await;

        connector_tx.send_replace(ConnectorState::new(0, Some(string_connector())));
        expect_insert(&mut stream, "10.0.0.1:8080").await;

        cache.remove_endpoints("c1");
        expect_remove(&mut stream, "10.0.0.1:8080").await;
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
    async fn endpoint_swap_emits_insert_then_remove() {
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
