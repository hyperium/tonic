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
//! Construction is synchronous even though the first connector only exists
//! once CDS arrives — `connect` awaits publication instead. That lets a load
//! balancer take the connector up front and start discovery before CDS
//! resolves.

use std::sync::Arc;

use tokio::sync::watch;

use crate::client::endpoint::{ClusterConfig, Connector, EndpointAddress, MakeConnector};
use crate::common::async_util::BoxFuture;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;

/// A type-erased per-cluster connector, as published by CDS.
type SharedConnector<S> = Arc<dyn Connector<Service = S> + Send + Sync>;

/// A [`Connector`] that tracks one cluster's CDS resource.
///
/// Holds the receiving end of a [`watch`] channel fed by a spawned task; the
/// task exits (dropping the sender) when the cluster leaves the cache.
pub(crate) struct ClusterConnector<S> {
    cluster_name: Arc<str>,
    rx: watch::Receiver<Option<SharedConnector<S>>>,
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
        let (tx, rx) = watch::channel(None);
        tokio::spawn(watch_cluster(
            cache,
            cluster_name.clone(),
            make_connector,
            #[cfg(feature = "_tls-any")]
            registry,
            tx,
        ));
        Self {
            cluster_name: Arc::from(cluster_name),
            rx,
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
                let current = rx.borrow_and_update().clone();
                if let Some(connector) = current {
                    return connector.connect(&addr).await;
                }
                if rx.changed().await.is_err() {
                    tracing::error!(
                        cluster = %cluster_name,
                        endpoint = %addr,
                        "cluster removed from cache before any CDS update produced a \
                         connector; parking the connect until discovery drops it",
                    );
                    // The cluster left the cache before publishing a connector.
                    // Its endpoints are removed too, so discovery drops this
                    // future; parking avoids inventing a bogus connection.
                    return std::future::pending().await;
                }
            }
        })
    }
}

/// Background task: republishes a connector on every accepted CDS update.
async fn watch_cluster<MC, S>(
    cache: Arc<XdsCache>,
    cluster_name: String,
    make_connector: Arc<MC>,
    #[cfg(feature = "_tls-any")] registry: Arc<CertProviderRegistry>,
    tx: watch::Sender<Option<SharedConnector<S>>>,
) where
    MC: MakeConnector<Service = S>,
{
    let mut cluster_watch = cache.watch_cluster(&cluster_name);

    while let Some(cluster) = cluster_watch.next().await {
        let cluster_config = ClusterConfig::from_resource(
            &cluster,
            #[cfg(feature = "_tls-any")]
            &registry,
        );
        match make_connector.make_connector(cluster_config) {
            Ok(connector) => {
                if tx.send(Some(connector)).is_err() {
                    return;
                }
            }
            Err(e) => tracing::warn!(
                cluster = %cluster_name,
                error = %e,
                "CDS update rejected; keeping previous connector",
            ),
        }
    }
}
