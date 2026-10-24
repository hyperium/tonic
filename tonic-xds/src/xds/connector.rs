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

//! CDS-driven [`Connector`] for a single cluster.
//!
//! [`ClusterConnector`] watches one cluster's CDS resource and asks a
//! [`MakeConnector`] for a fresh [`Connector`] on every update (e.g. when
//! `transport_socket` changes), publishing it to a [`watch`] channel. Each
//! `connect` call reads the latest published connector, so new connections
//! pick up the current security config while established ones are untouched.
//!
//! An update whose config fails validation is rejected: the previous connector
//! is kept and a warning is logged.
//!
//! Each connector is published with the cache generation of the CDS resource
//! it was built from, and removing the cluster withdraws it. Endpoint discovery
//! waits for the connector of its own EDS generation
//! ([`ClusterConnector::connector_for_generation`]), so endpoints of a re-added
//! cluster are never connected with the removed cluster's transport settings.
//! This relies on the resource manager removing a cluster's CDS and EDS
//! resources together, which keeps their generations in step.
//!
//! Construction is synchronous even though the first connector only exists
//! once CDS arrives — `connect` awaits publication instead. That lets a load
//! balancer take the connector up front and start discovery before CDS
//! resolves.

use std::cmp::Ordering;
use std::sync::Arc;

use tokio::sync::watch;

use crate::client::endpoint::{ClusterConfig, Connector, EndpointAddress, MakeConnector};
use crate::common::async_util::BoxFuture;
use crate::xds::cache::{CacheEvent, XdsCache};
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;

/// A type-erased per-cluster connector, as published by CDS.
pub(crate) type SharedConnector<S> = Arc<dyn Connector<Service = S> + Send + Sync>;

/// The connector built from a cluster's CDS resource, tagged with the cache
/// generation of that resource.
pub(crate) struct ConnectorState<S> {
    generation: u64,
    /// `None` until an update of `generation` is accepted, and again once the
    /// cluster is removed.
    connector: Option<SharedConnector<S>>,
}

impl<S> ConnectorState<S> {
    pub(crate) fn new(generation: u64, connector: Option<SharedConnector<S>>) -> Self {
        Self {
            generation,
            connector,
        }
    }
}

/// A [`Connector`] that tracks one cluster's CDS resource.
///
/// Holds the receiving end of a [`watch`] channel fed by a spawned task; the
/// task exits once every handle is dropped.
pub(crate) struct ClusterConnector<S> {
    cluster_name: Arc<str>,
    rx: watch::Receiver<ConnectorState<S>>,
}

impl<S> Clone for ClusterConnector<S> {
    fn clone(&self) -> Self {
        Self {
            cluster_name: self.cluster_name.clone(),
            rx: self.rx.clone(),
        }
    }
}

impl<S: Send + 'static> ClusterConnector<S> {
    /// Spawns the CDS watch for `cluster_name` and returns a connector backed
    /// by it.
    pub(crate) fn spawn<MC>(
        cache: Arc<XdsCache>,
        cluster_name: String,
        make_connector: Arc<MC>,
        #[cfg(feature = "_tls-any")] registry: Arc<CertProviderRegistry>,
    ) -> Self
    where
        MC: MakeConnector<Service = S>,
    {
        let (tx, rx) = watch::channel(ConnectorState::new(0, None));
        tokio::spawn(watch_cluster(
            cache,
            cluster_name.clone(),
            make_connector,
            #[cfg(feature = "_tls-any")]
            registry,
            tx,
        ));
        Self::from_receiver(cluster_name, rx)
    }

    /// A connector fed by `rx` rather than by a CDS watch.
    pub(crate) fn from_receiver(
        cluster_name: impl Into<Arc<str>>,
        rx: watch::Receiver<ConnectorState<S>>,
    ) -> Self {
        Self {
            cluster_name: cluster_name.into(),
            rx,
        }
    }

    /// Waits for the connector built from the cluster's CDS `generation`.
    ///
    /// Returns `None` once the cluster has moved past that generation (it was
    /// removed, and possibly re-added): endpoints of that generation are
    /// obsolete and must not be connected.
    pub(crate) async fn connector_for_generation(
        &self,
        generation: u64,
    ) -> Option<SharedConnector<S>> {
        let mut rx = self.rx.clone();
        loop {
            {
                let state = rx.borrow_and_update();
                match state.generation.cmp(&generation) {
                    Ordering::Greater => return None,
                    Ordering::Equal if state.connector.is_some() => {
                        return state.connector.clone();
                    }
                    Ordering::Equal | Ordering::Less => {}
                }
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }
}

impl<S: Send + 'static> Connector for ClusterConnector<S> {
    type Service = S;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        let mut rx = self.rx.clone();
        let cluster_name = self.cluster_name.clone();
        let addr = addr.clone();

        Box::pin(async move {
            loop {
                // Clone out of the borrow guard; it is not held across an await.
                let current = rx.borrow_and_update().connector.clone();
                if let Some(connector) = current {
                    return connector.connect(&addr).await;
                }
                if rx.changed().await.is_err() {
                    tracing::error!(
                        cluster = %cluster_name,
                        endpoint = %addr,
                        "cluster watch stopped without a connector; parking the \
                         connect until discovery drops it",
                    );
                    // The cache is gone, so discovery is shutting down and will
                    // drop this future; parking avoids inventing a bogus
                    // connection.
                    return std::future::pending().await;
                }
            }
        })
    }
}

/// Background task: republishes a connector on every accepted CDS update and
/// withdraws it when the cluster is removed.
async fn watch_cluster<MC, S>(
    cache: Arc<XdsCache>,
    cluster_name: String,
    make_connector: Arc<MC>,
    #[cfg(feature = "_tls-any")] registry: Arc<CertProviderRegistry>,
    tx: watch::Sender<ConnectorState<S>>,
) where
    MC: MakeConnector<Service = S>,
{
    let mut cluster_watch = cache.watch_cluster(&cluster_name);

    loop {
        // Removed clusters stay in the cache as tombstones, so the watch never
        // ends on its own; stop once nobody can observe the connector.
        let event = tokio::select! {
            _ = tx.closed() => return,
            event = cluster_watch.next_event() => event,
        };
        let (generation, cluster) = match event {
            None => return,
            Some(CacheEvent::Removed { generation }) => {
                tx.send_replace(ConnectorState::new(generation, None));
                continue;
            }
            Some(CacheEvent::Resource {
                generation,
                resource,
                ..
            }) => (generation, resource),
        };

        let cluster_config = ClusterConfig::from_resource(
            &cluster,
            #[cfg(feature = "_tls-any")]
            &registry,
        );
        match make_connector.make_connector(cluster_config) {
            Ok(connector) => {
                tx.send_replace(ConnectorState::new(generation, Some(connector)));
            }
            Err(e) => {
                // Only a connector of the same generation may be kept; a
                // re-added cluster waits for a valid update instead.
                tx.send_if_modified(|state| {
                    if state.generation == generation {
                        return false;
                    }
                    *state = ConnectorState::new(generation, None);
                    true
                });
                if tx.borrow().connector.is_some() {
                    tracing::warn!(
                        cluster = %cluster_name,
                        error = %e,
                        "CDS update rejected; keeping previous connector",
                    );
                } else {
                    tracing::warn!(
                        cluster = %cluster_name,
                        error = %e,
                        "CDS update rejected; endpoints wait for a valid update",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::cluster::{ClusterResource, LbPolicy};
    use std::time::Duration;
    use tower::BoxError;

    /// Connects each endpoint to its own address string.
    struct AddrConnector;

    impl Connector for AddrConnector {
        type Service = String;

        fn connect(&self, addr: &EndpointAddress) -> BoxFuture<String> {
            let addr = addr.to_string();
            Box::pin(async move { addr })
        }
    }

    struct TestMakeConnector;

    impl MakeConnector for TestMakeConnector {
        type Service = String;

        fn make_connector(
            &self,
            _cluster: ClusterConfig<'_>,
        ) -> Result<SharedConnector<String>, BoxError> {
            Ok(Arc::new(AddrConnector))
        }
    }

    fn cluster() -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: "c1".to_string(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
            circuit_breaking: Default::default(),
        })
    }

    #[tokio::test]
    async fn cluster_removal_withdraws_connector_until_readd() {
        let cache = Arc::new(XdsCache::new());
        cache.update_cluster("c1", cluster());
        let connector = ClusterConnector::spawn(
            cache.clone(),
            "c1".to_string(),
            Arc::new(TestMakeConnector),
            #[cfg(feature = "_tls-any")]
            Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            ),
        );
        assert!(connector.connector_for_generation(0).await.is_some());

        cache.remove_cluster("c1");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                connector.connector_for_generation(1)
            )
            .await
            .is_err(),
            "a removed cluster has no connector until it is re-added",
        );
        assert!(
            connector.connector_for_generation(0).await.is_none(),
            "endpoints from before the removal must not be connected",
        );

        cache.update_cluster("c1", cluster());
        assert!(connector.connector_for_generation(1).await.is_some());
        let addr = EndpointAddress::new("10.0.0.1", 8080);
        assert_eq!(connector.connect(&addr).await, "10.0.0.1:8080");
    }
}
