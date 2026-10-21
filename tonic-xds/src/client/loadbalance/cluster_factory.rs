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

//! [`ClusterChannelFactory`] backed by the in-crate [`LoadBalancer`].
//!
//! The counterpart to
//! [`TowerP2cClusterChannelFactory`](crate::client::lb::TowerP2cClusterChannelFactory).
//! Where that one hands Tower's balancer a stream of already-connected
//! endpoints, this one keeps discovery and connection separate: the balancer
//! receives bare addresses plus a [`Connector`](crate::client::endpoint::Connector),
//! so it owns the connect / eject / reconnect lifecycle that outlier detection
//! depends on.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tower::{BoxError, Service, load::Load};

use crate::client::cluster::{ClusterChannel, ClusterChannelFactory};
use crate::client::endpoint::MakeConnector;
use crate::client::loadbalance::adapter::ClusterChannelAdapter;
use crate::client::loadbalance::channel_state::ReadyChannel;
use crate::client::loadbalance::loadbalancer::LoadBalancer;
use crate::client::loadbalance::outcome::{OutcomeClassifier, OutcomeSource};
use crate::client::loadbalance::pickers::ChannelPicker;
use crate::client::loadbalance::pickers::p2c::P2cPicker;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::CertProviderRegistry;
use crate::xds::connector::ClusterConnector;
use crate::xds::endpoint_manager::discover_addresses;
use crate::xds::resource::outlier_detection::OutlierDetectionConfig;

/// Builds one [`LoadBalancer`] per cluster, fed by the xDS cache.
pub(crate) struct XdsLbClusterChannelFactory<MC> {
    cache: Arc<XdsCache>,
    make_connector: Arc<MC>,
    /// Transport-specific mapping of call results to outlier-detection
    /// verdicts; gRPC and plain-HTTP channels supply different ones.
    outcome_classifier: Arc<dyn OutcomeClassifier>,
    #[cfg(feature = "_tls-any")]
    cert_registry: Arc<CertProviderRegistry>,
}

impl<MC> XdsLbClusterChannelFactory<MC> {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(
        cache: Arc<XdsCache>,
        make_connector: MC,
        outcome_classifier: Arc<dyn OutcomeClassifier>,
        cert_registry: Arc<CertProviderRegistry>,
    ) -> Self {
        Self {
            cache,
            make_connector: Arc::new(make_connector),
            outcome_classifier,
            cert_registry,
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(
        cache: Arc<XdsCache>,
        make_connector: MC,
        outcome_classifier: Arc<dyn OutcomeClassifier>,
    ) -> Self {
        Self {
            cache,
            make_connector: Arc::new(make_connector),
            outcome_classifier,
        }
    }
}

impl<MC, Req> ClusterChannelFactory<Req, <MC::Service as Service<Req>>::Response>
    for XdsLbClusterChannelFactory<MC>
where
    MC: MakeConnector,
    MC::Service: Service<Req> + Load + Send + 'static,
    <MC::Service as Load>::Metric: PartialOrd,
    <MC::Service as Service<Req>>::Response: Send + 'static + OutcomeSource,
    <MC::Service as Service<Req>>::Error: Into<BoxError>,
    <MC::Service as Service<Req>>::Future: Send + 'static,
    Req: Send + 'static,
{
    fn make(
        &self,
        cluster_name: &str,
    ) -> ClusterChannel<Req, <MC::Service as Service<Req>>::Response> {
        let connector = Arc::new(ClusterConnector::spawn(
            self.cache.clone(),
            cluster_name.to_string(),
            self.make_connector.clone(),
            #[cfg(feature = "_tls-any")]
            self.cert_registry.clone(),
        ));
        let discovery = discover_addresses(self.cache.watch_endpoints(cluster_name));

        // TODO: apply CDS updates in place rather than rebuilding this channel.
        // The balancer owns the live connections, and the ejection counters are
        // `Arc`s shared with each `ReadyChannel`, so rebuilding drops both — and
        // a fresh discovery stream replays every endpoint as a `Change::Insert`,
        // which `poll_discover` handles by dropping the ready channel and
        // reconnecting. The shape that fits: a per-cluster config watch drained
        // by `LoadBalancer::poll_ready` alongside discovery, applying
        //   * picker — seed the replacement via `on_members_changed(&members)`
        //     before swapping, since ring-hash builds its ring from that set.
        //     Blocked on `ClusterResource.lb_policy` modelling anything past
        //     round-robin / least-request.
        //   * outlier detection — `config.store` covers most fields because the
        //     sweep re-reads them, but `interval` is captured by `spawn_actor`
        //     and the actor only exists when the config was enabled at
        //     construction, so both need an actor respawn or drop on change.
        let picker: Arc<dyn ChannelPicker<ReadyChannel<MC::Service>, Req> + Send + Sync> =
            Arc::new(P2cPicker);
        // `default()` is the disabled config, so no ejection actor is spawned.
        let config = Arc::new(ArcSwap::from_pointee(OutlierDetectionConfig::default()));

        let lb = LoadBalancer::new(
            discovery,
            connector,
            picker,
            config,
            self.outcome_classifier.clone(),
        );
        ClusterChannel::from_service(ClusterChannelAdapter::new(lb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::EndpointAddress;
    use crate::client::loadbalance::outcome::GrpcOutcomeClassifier;
    use crate::testutil::grpc::{TestServer, spawn_greeter_server};
    use crate::xds::cluster_discovery::GrpcMakeConnector;
    use crate::xds::resource::cluster::{ClusterResource, LbPolicy};
    use crate::xds::resource::endpoints::{
        EndpointsResource, HealthStatus, LocalityEndpoints, ResolvedEndpoint,
    };
    use std::collections::HashSet;
    use tonic::body::Body as TonicBody;
    use tower::ServiceExt;

    const CLUSTER: &str = "test-cluster";

    fn plaintext_cluster() -> Arc<ClusterResource> {
        Arc::new(ClusterResource {
            name: CLUSTER.to_string(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
        })
    }

    fn endpoints_for(servers: &[TestServer]) -> Arc<EndpointsResource> {
        Arc::new(EndpointsResource {
            cluster_name: CLUSTER.to_string(),
            localities: vec![LocalityEndpoints {
                locality: None,
                endpoints: servers
                    .iter()
                    .map(|s| ResolvedEndpoint {
                        address: EndpointAddress::from(s.addr),
                        additional_addresses: vec![],
                        health_status: HealthStatus::Healthy,
                        load_balancing_weight: 1,
                    })
                    .collect(),
                load_balancing_weight: 100,
                priority: 0,
            }],
        })
    }

    fn grpc_factory(cache: Arc<XdsCache>) -> XdsLbClusterChannelFactory<GrpcMakeConnector> {
        XdsLbClusterChannelFactory::new(
            cache,
            GrpcMakeConnector::new(),
            Arc::new(GrpcOutcomeClassifier),
            #[cfg(feature = "_tls-any")]
            Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            ),
        )
    }

    /// Issues `count` unary requests through `channel`, returning the set of
    /// server names that answered. The greeter replies `"{server}: {name}"`.
    async fn call_servers(
        channel: &ClusterChannel<http::Request<TonicBody>, http::Response<TonicBody>>,
        count: usize,
    ) -> HashSet<String> {
        use crate::testutil::grpc::{GreeterClient, HelloRequest};

        let mut seen = HashSet::new();
        let mut client = GreeterClient::new(channel.clone());
        for i in 0..count {
            let response = client
                .say_hello(HelloRequest {
                    name: format!("req-{i}"),
                })
                .await
                .expect("request should reach an endpoint");
            let message = response.into_inner().message;
            seen.insert(message.split(':').next().unwrap().to_string());
        }
        seen
    }

    #[tokio::test]
    async fn balances_requests_across_cluster_endpoints() {
        let servers = vec![
            spawn_greeter_server("server-0", None, None).await.unwrap(),
            spawn_greeter_server("server-1", None, None).await.unwrap(),
        ];

        let cache = Arc::new(XdsCache::new());
        cache.update_cluster(CLUSTER, plaintext_cluster());
        cache.update_endpoints(CLUSTER, endpoints_for(&servers));

        let factory = grpc_factory(cache);
        let channel: ClusterChannel<http::Request<TonicBody>, http::Response<TonicBody>> =
            factory.make(CLUSTER);

        let seen = call_servers(&channel, 50).await;
        assert_eq!(
            seen.len(),
            2,
            "both endpoints should receive traffic, saw {seen:?}",
        );
    }

    /// The balancer is handed a connector up front and only resolves it when
    /// CDS lands, so a channel built before the cluster exists must still work.
    #[tokio::test]
    async fn serves_once_cds_arrives_after_construction() {
        let servers = vec![spawn_greeter_server("late", None, None).await.unwrap()];

        let cache = Arc::new(XdsCache::new());
        let factory = grpc_factory(cache.clone());
        let mut channel: ClusterChannel<http::Request<TonicBody>, http::Response<TonicBody>> =
            factory.make(CLUSTER);

        cache.update_cluster(CLUSTER, plaintext_cluster());
        cache.update_endpoints(CLUSTER, endpoints_for(&servers));

        channel.ready().await.expect("channel becomes ready");
        let seen = call_servers(&channel, 1).await;
        assert_eq!(seen, HashSet::from(["late".to_string()]));
    }
}
