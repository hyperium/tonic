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

use crate::TonicCallCredentials;
use crate::XdsUri;
use crate::client::retry::{GrpcRetryPolicy, RetryLayer};
use crate::client::route::{PreRouteInterceptor, Router, XdsRoutingLayer};
use crate::xds::bootstrap::{BootstrapConfig, BootstrapError};
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::{CertProviderError, CertProviderRegistry, CertificateProvider};
use crate::xds::resource_manager::XdsResourceManager;
use crate::xds::routing::XdsRouter;
use http::Request;
#[cfg(feature = "_tls-any")]
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::task::{Context, Poll};
use tonic::{body::Body as TonicBody, client::GrpcService};
use tower::{BoxError, Service, ServiceBuilder, util::BoxCloneSyncService};
use xds_client::{
    ClientConfig, MetricsRecorder, Node, ProstCodec, TokioRuntime, TonicTransportBuilder, XdsClient,
};

cfg_tower_lb! {
    use crate::client::cluster::ClusterClientRegistryGrpc;
    use crate::client::endpoint::{EndpointAddress, EndpointChannel};
    use crate::client::lb::{ClusterDiscovery, XdsLbService};
    use crate::xds::cluster_discovery::XdsClusterDiscovery;
    use tonic::transport::channel::Channel;
}

cfg_tonic_xds_lb! {
    use crate::client::loadbalance::service::XdsLoadBalanceService;
}

/// Configuration for building [`XdsChannel`] / [`XdsChannelGrpc`].
#[derive(Clone, Debug)]
pub struct XdsChannelConfig {
    target_uri: XdsUri,
    bootstrap: Option<BootstrapConfig>,
    call_creds: Option<Arc<dyn TonicCallCredentials>>,
}

impl XdsChannelConfig {
    /// Creates a new config with the given target URI.
    #[must_use]
    pub fn new(target_uri: XdsUri) -> Self {
        Self {
            target_uri,
            bootstrap: None,
            call_creds: None,
        }
    }

    /// Sets the bootstrap configuration.
    ///
    /// If not set, the builder falls back to loading from environment
    /// variables (`GRPC_XDS_BOOTSTRAP` or `GRPC_XDS_BOOTSTRAP_CONFIG`).
    #[must_use]
    pub fn with_bootstrap(mut self, bootstrap: BootstrapConfig) -> Self {
        self.bootstrap = Some(bootstrap);
        self
    }

    /// Eagerly loads bootstrap configuration from environment variables.
    ///
    /// This is optional — [`XdsChannelBuilder::build_grpc_channel`] falls back
    /// to env vars automatically if no bootstrap is set. Use this method when
    /// you want to surface bootstrap errors at config time rather than build time.
    ///
    /// Reads from `GRPC_XDS_BOOTSTRAP` (file path) first, then falls back to
    /// `GRPC_XDS_BOOTSTRAP_CONFIG` (inline JSON).
    pub fn with_bootstrap_from_env(mut self) -> Result<Self, BootstrapError> {
        self.bootstrap = Some(BootstrapConfig::from_env()?);
        Ok(self)
    }

    /// Set per-stream call credentials for the ADS stream (e.g. `google_default`).
    ///
    /// Attached on each (re)connect, only over a secure channel; over an insecure
    /// channel, stream creation fails. Not refreshed mid-stream.
    pub fn with_call_credentials(mut self, creds: Arc<dyn TonicCallCredentials>) -> Self {
        self.call_creds = Some(creds);
        self
    }
}

/// Errors that can occur when building an [`XdsChannel`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Bootstrap configuration could not be loaded.
    #[error("bootstrap: {0}")]
    Bootstrap(#[from] BootstrapError),
    /// A `certificate_providers` entry in the bootstrap failed to initialize.
    #[cfg(feature = "_tls-any")]
    #[error("certificate provider: {0}")]
    CertProvider(#[from] CertProviderError),
}

/// Holds owned resources whose background tasks must live as long as the channel.
///
/// Stored as `Option<Arc<...>>` on [`XdsChannel`] so clones share ownership
/// cheaply. When the last clone drops, the resource manager cascade task and
/// ADS worker are aborted. The `XdsCache` is kept alive separately by
/// `XdsClusterDiscovery` in the service stack.
struct XdsChannelResources {
    _resource_manager: XdsResourceManager,
    _xds_client: XdsClient,
}

/// `XdsChannel` is an xDS-capable [`tower::Service`] implementation.
///
/// It routes requests according to the xDS configuration that it fetches from the xDS management server.
/// The routing implementation is based on the [Google gRPC xDS features](https://grpc.github.io/grpc/core/md_doc_grpc_xds_features.html).
pub struct XdsChannel<S> {
    config: Arc<XdsChannelConfig>,
    inner: S,
    /// Keeps background tasks alive. `None` when built from parts in tests.
    _resources: Option<Arc<XdsChannelResources>>,
}

#[allow(clippy::missing_fields_in_debug)]
impl<S> Debug for XdsChannel<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdsChannel")
            .field("config", &self.config)
            .finish()
    }
}

impl<S: Clone> Clone for XdsChannel<S> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            inner: self.inner.clone(),
            _resources: self._resources.clone(),
        }
    }
}

impl<Req, S> Service<Req> for XdsChannel<S>
where
    S: Service<Req, Error = BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Req) -> Self::Future {
        self.inner.call(request)
    }
}

/// A [`tonic::client::GrpcService`] implementation that can route and load-balance
/// gRPC requests based on xDS configuration.
///
/// `Send + Sync + Clone`. Cloning is cheap (the inner service stack is
/// reference-counted); callers that need exclusive access for
/// [`tower::Service::call`] should clone per call site rather than share a
/// single instance through a lock.
pub type XdsChannelGrpc =
    BoxCloneSyncService<http::Request<TonicBody>, http::Response<TonicBody>, BoxError>;

// Static assertions: XdsChannelGrpc implements GrpcService and is shareable
// across tasks (Send + Sync).
const _: fn() = || {
    fn assert_grpc_service<T: GrpcService<TonicBody>>() {}
    fn assert_send_sync<T: Send + Sync>() {}
    assert_grpc_service::<XdsChannelGrpc>();
    assert_send_sync::<XdsChannelGrpc>();
};

/// Builder for creating an [`XdsChannel`] or [`XdsChannelGrpc`].
#[derive(Clone)]
pub struct XdsChannelBuilder {
    config: Arc<XdsChannelConfig>,
    recorder: Option<Arc<dyn MetricsRecorder>>,
    pre_route: Option<Arc<dyn PreRouteInterceptor>>,
    #[cfg(feature = "_tls-any")]
    cert_providers: HashMap<String, Arc<dyn CertificateProvider>>,
}

impl Debug for XdsChannelBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("XdsChannelBuilder");
        s.field("config", &self.config)
            .field(
                "recorder",
                &self
                    .recorder
                    .as_deref()
                    .map_or("None", |r| std::any::type_name_of_val(r)),
            )
            .field(
                "pre_route",
                &self
                    .pre_route
                    .as_deref()
                    .map_or("None", |i| std::any::type_name_of_val(i)),
            );
        #[cfg(feature = "_tls-any")]
        s.field(
            "cert_providers",
            &self.cert_providers.keys().collect::<Vec<_>>(),
        );
        s.finish()
    }
}

impl XdsChannelBuilder {
    /// Creates a builder from a channel configuration.
    #[must_use]
    pub fn new(config: XdsChannelConfig) -> Self {
        Self {
            config: Arc::new(config),
            recorder: None,
            pre_route: None,
            #[cfg(feature = "_tls-any")]
            cert_providers: HashMap::new(),
        }
    }

    /// Sets the [`MetricsRecorder`] backend that receives the gRFC A78 xDS
    /// client metrics emitted by the underlying [`XdsClient`].
    ///
    /// By default no recorder is configured and metric emission is skipped.
    /// With the `otel` feature, `with_otel_metrics` provides a one-call
    /// OpenTelemetry setup.
    #[must_use]
    pub fn with_metrics_recorder(mut self, recorder: Arc<dyn MetricsRecorder>) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// Registers a [`PreRouteInterceptor`] that runs before route selection.
    ///
    /// The routing layer takes a `RouteConfiguration` snapshot for each request;
    /// when an interceptor is registered, it is invoked inline against that
    /// snapshot before matching, and may mutate request headers (using the
    /// config's [`RouteConfigMetadata`](crate::RouteConfigMetadata)) so the
    /// router matches on the result. This enables config-driven request
    /// transformation such as computing a partition/shard key and injecting a
    /// routing header. The interceptor and the router observe the same snapshot.
    #[must_use]
    pub fn with_pre_route_interceptor(mut self, interceptor: Arc<dyn PreRouteInterceptor>) -> Self {
        self.pre_route = Some(interceptor);
        self
    }

    /// Registers a custom [`CertificateProvider`] under an xDS certificate
    /// provider instance name, resolved by CDS `UpstreamTlsContext` references.
    /// Shadows a bootstrap `file_watcher` instance of the same name.
    ///
    /// [`CertificateProvider`]: crate::CertificateProvider
    #[cfg(feature = "_tls-any")]
    #[must_use]
    pub fn with_certificate_provider(
        mut self,
        instance_name: impl Into<String>,
        provider: Arc<dyn CertificateProvider>,
    ) -> Self {
        self.cert_providers.insert(instance_name.into(), provider);
        self
    }

    /// Emits the gRFC A78 xDS client metrics through an OpenTelemetry `Meter`.
    ///
    /// Convenience wrapper over
    /// [`with_metrics_recorder`](Self::with_metrics_recorder) that installs an
    /// [`OtelMetricsRecorder`](xds_client_opentelemetry::OtelMetricsRecorder) from
    /// the companion `xds-client-opentelemetry` crate. Requires the `otel` feature.
    #[cfg(feature = "otel")]
    #[must_use]
    pub fn with_otel_metrics(self, meter: opentelemetry::metrics::Meter) -> Self {
        self.with_metrics_recorder(Arc::new(
            xds_client_opentelemetry::OtelMetricsRecorder::new(meter),
        ))
    }

    fn build_tonic_grpc_channel(&self) -> Result<XdsChannelGrpc, BuildError> {
        let bootstrap = match self.config.bootstrap.clone() {
            Some(b) => b,
            None => BootstrapConfig::from_env()?,
        };

        let listener_name = self.config.target_uri.target.clone();

        let server_uri = bootstrap.server_uri().to_owned();

        #[allow(unused_mut)]
        let mut transport_builder = TonicTransportBuilder::new();
        #[cfg(any(feature = "tls-ring", feature = "tls-aws-lc"))]
        if bootstrap.use_tls() {
            transport_builder = transport_builder
                .with_tls_config(tonic::transport::ClientTlsConfig::new().with_enabled_roots());
        }
        #[cfg(not(any(feature = "tls-ring", feature = "tls-aws-lc")))]
        if bootstrap.use_tls() {
            return Err(BuildError::Bootstrap(BootstrapError::Validation(
                "TLS requested by bootstrap but no TLS feature enabled \
                 (enable tls-ring or tls-aws-lc)"
                    .into(),
            )));
        }

        if let Some(creds) = self.config.call_creds.clone() {
            transport_builder = transport_builder.with_call_credentials(creds);
        }

        #[cfg(feature = "_tls-any")]
        let cert_provider_registry = Arc::new(CertProviderRegistry::from_bootstrap(
            &bootstrap.certificate_providers,
            self.cert_providers.clone(),
        )?);

        let node = Node::try_from(bootstrap.node)?;
        let client_config =
            ClientConfig::new(node, &server_uri).with_target(self.config.target_uri.to_string());
        let mut client_builder =
            XdsClient::builder(client_config, transport_builder, ProstCodec, TokioRuntime);
        if let Some(recorder) = self.recorder.clone() {
            client_builder = client_builder.with_metrics_recorder(recorder);
        }
        let xds_client = client_builder.build();

        let cache = Arc::new(XdsCache::new());
        let resource_manager =
            XdsResourceManager::new(xds_client.clone(), cache.clone(), listener_name);

        Ok(self.build_from_cache(
            cache,
            #[cfg(feature = "_tls-any")]
            cert_provider_registry,
            xds_client,
            resource_manager,
        ))
    }

    /// Internal builder that wires the service stack from a pre-built cache.
    ///
    /// Separated from `build_tonic_grpc_channel` so tests can inject a
    /// disconnected `XdsClient` and pre-populated cache.
    fn build_from_cache(
        &self,
        cache: Arc<XdsCache>,
        #[cfg(feature = "_tls-any")] cert_provider_registry: Arc<CertProviderRegistry>,
        xds_client: XdsClient,
        resource_manager: XdsResourceManager,
    ) -> XdsChannelGrpc {
        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));

        #[cfg(not(feature = "tonic-xds-lb"))]
        let lb_service = {
            #[cfg(feature = "_tls-any")]
            let discovery: Arc<
                dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
            > = Arc::new(XdsClusterDiscovery::new(cache, cert_provider_registry));
            #[cfg(not(feature = "_tls-any"))]
            let discovery: Arc<
                dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
            > = Arc::new(XdsClusterDiscovery::new(cache));
            let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
            XdsLbService::new(cluster_registry, discovery)
        };

        #[cfg(feature = "tonic-xds-lb")]
        let lb_service = XdsLoadBalanceService::new(
            cache,
            #[cfg(feature = "_tls-any")]
            cert_provider_registry,
        );

        let retry_policy = GrpcRetryPolicy::default();

        let resources = Arc::new(XdsChannelResources {
            _resource_manager: resource_manager,
            _xds_client: xds_client,
        });

        let routing_layer = XdsRoutingLayer::new(router, self.pre_route.clone(), self.authority());
        let retry_layer = RetryLayer::new(retry_policy);
        let inner = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);

        BoxCloneSyncService::new(XdsChannel {
            config: self.config.clone(),
            inner,
            _resources: Some(resources),
        })
    }

    /// Builds an `XdsChannelGrpc`, which is a type-erased gRPC channel.
    // TODO: Support HTTP and other channel types (not just gRPC). This will
    // require a generic `build()` or separate `build_http_channel()` method.
    pub fn build_grpc_channel(&self) -> Result<XdsChannelGrpc, BuildError> {
        self.build_tonic_grpc_channel()
    }

    /// Test-only: builds an `XdsChannelGrpc` for the `tower-lb` backend
    /// (router + `XdsLbService`, no resource manager) from a pre-populated
    /// cache. Both backend constructors are compiled in test builds (see the
    /// `any(test, …)` module gates), so the channel tests run against each.
    #[cfg(test)]
    pub(crate) fn build_grpc_channel_from_cache_tower(
        &self,
        cache: Arc<XdsCache>,
        retry_policy: GrpcRetryPolicy,
    ) -> XdsChannelGrpc {
        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));
        #[cfg(feature = "_tls-any")]
        let discovery: Arc<
            dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
        > = Arc::new(XdsClusterDiscovery::new(
            cache,
            Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            ),
        ));
        #[cfg(not(feature = "_tls-any"))]
        let discovery: Arc<
            dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
        > = Arc::new(XdsClusterDiscovery::new(cache));
        let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
        let lb_service = XdsLbService::new(cluster_registry, discovery);

        let routing_layer = XdsRoutingLayer::new(router, None, self.authority());
        let retry_layer = RetryLayer::new(retry_policy);
        let inner = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);
        BoxCloneSyncService::new(XdsChannel {
            config: self.config.clone(),
            inner,
            _resources: None,
        })
    }

    /// Test-only: builds an `XdsChannelGrpc` for the `tonic-xds-lb` backend
    /// (router + `XdsLoadBalanceService`, no resource manager) from a
    /// pre-populated cache.
    #[cfg(test)]
    pub(crate) fn build_grpc_channel_from_cache_xds(
        &self,
        cache: Arc<XdsCache>,
        retry_policy: GrpcRetryPolicy,
    ) -> XdsChannelGrpc {
        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));
        let lb_service = XdsLoadBalanceService::new(
            cache,
            #[cfg(feature = "_tls-any")]
            Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            ),
        );

        let routing_layer = XdsRoutingLayer::new(router, None, self.authority());
        let retry_layer = RetryLayer::new(retry_policy);
        let inner = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);
        BoxCloneSyncService::new(XdsChannel {
            config: self.config.clone(),
            inner,
            _resources: None,
        })
    }

    /// Builds an `XdsChannelGrpc` from the given router, cluster discovery, retry
    /// policy, and optional pre-route interceptor.
    #[cfg(test)]
    pub(crate) fn build_grpc_channel_from_parts(
        &self,
        router: Arc<dyn Router>,
        discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>>,
        retry_policy: GrpcRetryPolicy,
        interceptor: Option<Arc<dyn PreRouteInterceptor>>,
    ) -> XdsChannelGrpc {
        let routing_layer = XdsRoutingLayer::new(router, interceptor, self.authority());
        let retry_layer = RetryLayer::new(retry_policy);
        let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
        let lb_service = XdsLbService::new(cluster_registry, discovery);
        let inner = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);
        BoxCloneSyncService::new(XdsChannel {
            config: self.config.clone(),
            inner,
            _resources: None,
        })
    }

    /// Channel-level authority used as the routing key for matching against
    /// `VirtualHost.domains` in RDS.
    fn authority(&self) -> Arc<str> {
        Arc::from(self.config.target_uri.target.as_str())
    }
}

/// Feature-agnostic test helpers shared by both LB backends' channel tests.
#[cfg(test)]
mod test_support {
    use super::{XdsChannelConfig, XdsChannelGrpc};
    use crate::XdsUri;
    use crate::client::endpoint::EndpointAddress;
    use crate::testutil::grpc::{GreeterClient, HelloRequest, TestServer, spawn_greeter_server};
    use crate::xds::resource::EndpointsResource;
    use crate::xds::resource::route_config::RouteConfigResource;
    use std::collections::HashMap;
    use std::sync::Arc;

    pub(super) fn test_config() -> XdsChannelConfig {
        XdsChannelConfig::new(XdsUri::parse("xds:///test-service").unwrap())
    }

    /// Spawns `count` greeter servers named `server-0` .. `server-{count-1}`.
    pub(super) async fn setup_grpc_servers(count: usize) -> Vec<TestServer> {
        let mut servers = Vec::new();
        for i in 0..count {
            let server = spawn_greeter_server(&format!("server-{i}"), None)
                .await
                .expect("Failed to spawn gRPC server");
            servers.push(server);
        }
        servers
    }

    /// A minimal plaintext `ClusterResource`.
    pub(super) fn make_test_cluster(
        cluster_name: &str,
    ) -> Arc<crate::xds::resource::ClusterResource> {
        use crate::xds::resource::cluster::{ClusterResource, LbPolicy};
        Arc::new(ClusterResource {
            name: cluster_name.to_string(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
        })
    }

    /// A `RouteConfigResource` that routes all traffic to `cluster_name`.
    pub(super) fn make_test_route_config(cluster_name: &str) -> Arc<RouteConfigResource> {
        use crate::xds::resource::route_config::{
            PathSpecifierConfig, RouteConfig, RouteConfigAction, RouteConfigMatch,
            VirtualHostConfig,
        };
        Arc::new(RouteConfigResource {
            name: "test-route".to_string(),
            virtual_hosts: vec![VirtualHostConfig {
                name: "default".to_string(),
                domains: vec!["*".to_string()],
                routes: vec![RouteConfig {
                    match_criteria: RouteConfigMatch {
                        path_specifier: PathSpecifierConfig::Prefix(String::new()),
                        headers: vec![],
                        case_sensitive: false,
                        match_fraction: None,
                    },
                    action: RouteConfigAction::Cluster(cluster_name.to_string()),
                }],
            }],
            metadata: Default::default(),
        })
    }

    /// An `EndpointsResource` built from test server addresses.
    pub(super) fn make_test_endpoints(
        cluster_name: &str,
        servers: &[TestServer],
    ) -> Arc<EndpointsResource> {
        use crate::xds::resource::endpoints::{HealthStatus, LocalityEndpoints, ResolvedEndpoint};
        Arc::new(EndpointsResource {
            cluster_name: cluster_name.to_string(),
            localities: vec![LocalityEndpoints {
                locality: None,
                endpoints: servers
                    .iter()
                    .map(|s| ResolvedEndpoint {
                        address: EndpointAddress::from(s.addr),
                        health_status: HealthStatus::Healthy,
                        load_balancing_weight: 1,
                    })
                    .collect(),
                load_balancing_weight: 100,
                priority: 0,
            }],
        })
    }

    /// Sends `num_requests` gRPC requests and returns
    /// `(successful, error_types, per_server_counts)`. The message format is
    /// `"{server-name}: {request}"`, so `per_server_counts` reflects the LB
    /// distribution.
    pub(super) async fn send_grpc_requests(
        mut grpc_client: GreeterClient<XdsChannelGrpc>,
        num_requests: usize,
    ) -> (usize, HashMap<String, usize>, HashMap<String, usize>) {
        let mut successful_requests = 0;
        let mut error_types = HashMap::new();
        let mut server_counts = HashMap::new();

        for i in 0..num_requests {
            let request_timeout = tokio::time::Duration::from_secs(3);
            let request_future = grpc_client.say_hello(HelloRequest {
                name: format!("test-request-{i}"),
            });

            match tokio::time::timeout(request_timeout, request_future).await {
                Ok(Ok(response)) => {
                    successful_requests += 1;
                    let message = response.into_inner().message;
                    if let Some(server_name) = message.split(':').next() {
                        *server_counts.entry(server_name.to_string()).or_insert(0) += 1;
                    }
                }
                Ok(Err(e)) => {
                    let error_type = format!("{e:?}").chars().take(80).collect::<String>();
                    *error_types.entry(error_type).or_insert(0) += 1;
                }
                Err(_) => {
                    *error_types.entry("Timeout".to_string()).or_insert(0) += 1;
                    if error_types.get("Timeout").unwrap_or(&0) > &2 {
                        break;
                    }
                }
            }
        }

        (successful_requests, error_types, server_counts)
    }
}

#[cfg(test)]
mod tests {
    use super::XdsChannelBuilder;
    use super::XdsChannelGrpc;
    use super::test_support::{
        make_test_cluster, make_test_endpoints, make_test_route_config, send_grpc_requests,
        setup_grpc_servers, test_config,
    };
    use crate::client::retry::{GrpcRetryClassifier, GrpcRetryPolicy, RetryConfig};
    use crate::testutil::grpc::{GreeterClient, HelloRequest};
    use crate::xds::cache::XdsCache;
    use crate::{XdsChannelConfig, XdsUri};
    use std::sync::Arc;

    /// A cache-backed channel constructor for a specific LB backend.
    type BackendCtor = fn(&XdsChannelBuilder, Arc<XdsCache>, GrpcRetryPolicy) -> XdsChannelGrpc;

    /// Both LB backends are compiled in test builds (see the `any(test, …)`
    /// module gates), so every channel test below runs against each.
    fn backends() -> [(&'static str, BackendCtor); 2] {
        [
            (
                "tower-lb",
                XdsChannelBuilder::build_grpc_channel_from_cache_tower,
            ),
            (
                "tonic-xds-lb",
                XdsChannelBuilder::build_grpc_channel_from_cache_xds,
            ),
        ]
    }

    /// Power-of-two-choices distribution: with a pre-populated cache, requests
    /// are routed and load-balanced roughly evenly across all backends.
    #[tokio::test]
    async fn test_xds_channel_grpc_with_p2c_lb() {
        for (backend, build) in backends() {
            let cluster_name = "test-cluster";
            let num_requests = 1000;
            let num_servers = 5;
            let servers = setup_grpc_servers(num_servers).await;

            let cache = Arc::new(XdsCache::new());
            cache.update_route_config(make_test_route_config(cluster_name));
            cache.update_cluster(cluster_name, make_test_cluster(cluster_name));
            cache.update_endpoints(cluster_name, make_test_endpoints(cluster_name, &servers));

            let channel = build(
                &XdsChannelBuilder::new(test_config()),
                cache,
                GrpcRetryPolicy::default(),
            );
            let client = GreeterClient::new(channel);

            let (successful, error_types, server_counts) =
                send_grpc_requests(client, num_requests).await;

            assert_eq!(
                successful, num_requests,
                "[{backend}] expected 100% success. Errors: {error_types:?}",
            );
            assert!(
                error_types.is_empty(),
                "[{backend}] expected no errors: {error_types:?}",
            );
            assert_eq!(
                server_counts.len(),
                num_servers,
                "[{backend}] all {num_servers} servers should receive traffic: {server_counts:?}",
            );

            let expected = num_requests / num_servers;
            let min = (expected as f64 / 1.5) as usize;
            let max = (expected as f64 * 1.5) as usize;
            for (server, count) in &server_counts {
                assert!(
                    *count >= min && *count <= max,
                    "[{backend}] server {server} received {count}, expected ~{expected} (±1.5x): {server_counts:?}",
                );
            }

            for server in servers {
                let _ = server.shutdown.send(());
                let _ = server.handle.await;
            }
        }
    }

    /// The retry layer retries UNAVAILABLE and succeeds on the second attempt.
    #[tokio::test]
    async fn test_retry_once_on_unavailable() {
        use crate::testutil::grpc::spawn_fail_first_n_server;

        for (backend, build) in backends() {
            let cluster_name = "test-cluster";
            // Server fails the first request with UNAVAILABLE, succeeds on retry.
            let server = spawn_fail_first_n_server("retry-server", 1)
                .await
                .expect("Failed to spawn server");
            let servers = vec![server];

            let cache = Arc::new(XdsCache::new());
            cache.update_route_config(make_test_route_config(cluster_name));
            cache.update_cluster(cluster_name, make_test_cluster(cluster_name));
            cache.update_endpoints(cluster_name, make_test_endpoints(cluster_name, &servers));

            let retry_policy = GrpcRetryPolicy::new(
                RetryConfig::new().num_retries(1),
                GrpcRetryClassifier {
                    retry_on: vec![tonic::Code::Unavailable],
                },
            );
            let channel = build(&XdsChannelBuilder::new(test_config()), cache, retry_policy);
            let mut client = GreeterClient::new(channel);

            let response = client
                .say_hello(HelloRequest {
                    name: "retry-test".to_string(),
                })
                .await
                .unwrap_or_else(|e| {
                    panic!("[{backend}] request should succeed after retry: {e:?}")
                });
            assert_eq!(
                response.into_inner().message,
                "retry-server: retry-test",
                "[{backend}]",
            );

            for server in servers {
                let _ = server.shutdown.send(());
                let _ = server.handle.await;
            }
        }
    }

    /// Endpoint changes in the cache are picked up dynamically while serving.
    #[tokio::test]
    async fn test_xds_channel_handles_dynamic_endpoint_updates() {
        for (backend, build) in backends() {
            let cluster_name = "test-cluster";
            let servers = setup_grpc_servers(2).await;

            let cache = Arc::new(XdsCache::new());
            cache.update_route_config(make_test_route_config(cluster_name));
            cache.update_cluster(cluster_name, make_test_cluster(cluster_name));
            // Start with only the first server.
            cache.update_endpoints(
                cluster_name,
                make_test_endpoints(cluster_name, &servers[..1]),
            );

            let channel = build(
                &XdsChannelBuilder::new(test_config()),
                cache.clone(),
                GrpcRetryPolicy::default(),
            );
            let client = GreeterClient::new(channel.clone());

            // Phase 1: all traffic goes to server-0.
            let (successful, _, server_counts) = send_grpc_requests(client, 50).await;
            assert_eq!(successful, 50, "[{backend}]");
            assert_eq!(
                server_counts.len(),
                1,
                "[{backend}] only 1 server should receive traffic before update: {server_counts:?}",
            );

            // Add second server.
            cache.update_endpoints(cluster_name, make_test_endpoints(cluster_name, &servers));
            // Give the endpoint diff loop time to process the update.
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Phase 2: traffic should go to both servers.
            let client2 = GreeterClient::new(channel);
            let (successful, _, server_counts) = send_grpc_requests(client2, 200).await;
            assert_eq!(successful, 200, "[{backend}]");
            assert_eq!(
                server_counts.len(),
                2,
                "[{backend}] both servers should receive traffic after update: {server_counts:?}",
            );

            for server in servers {
                let _ = server.shutdown.send(());
                let _ = server.handle.await;
            }
        }
    }

    #[test]
    fn config_stores_call_credentials() {
        #[derive(Debug)]
        struct DummyCreds;
        #[tonic::async_trait]
        impl crate::TonicCallCredentials for DummyCreds {
            async fn get_request_metadata(
                &self,
                _metadata: &mut tonic::metadata::MetadataMap,
            ) -> Result<(), tonic::Status> {
                Ok(())
            }
        }
        let config = XdsChannelConfig::new(XdsUri::parse("xds:///svc").unwrap())
            .with_call_credentials(std::sync::Arc::new(DummyCreds));
        assert!(config.call_creds.is_some());
    }

    /// Smoke test: building the full stack (with a disconnected client) does
    /// not panic. Uses the production `build_from_cache`, which selects the LB
    /// backend by feature.
    #[tokio::test]
    async fn test_build_from_cache_smoke() {
        use crate::xds::resource_manager::XdsResourceManager;

        let cache = Arc::new(XdsCache::new());
        let xds_client = xds_client::XdsClient::disconnected();
        let resource_manager =
            XdsResourceManager::new(xds_client.clone(), cache.clone(), "test-listener".into());

        let builder = XdsChannelBuilder::new(test_config());

        #[cfg(feature = "_tls-any")]
        let _channel = {
            use crate::xds::cert_provider::CertProviderRegistry;
            let registry =
                Arc::new(CertProviderRegistry::from_bootstrap(&Default::default(), Default::default()).unwrap());
            builder.build_from_cache(cache, registry, xds_client, resource_manager)
        };
        #[cfg(not(feature = "_tls-any"))]
        let _channel = builder.build_from_cache(cache, xds_client, resource_manager);
        // Construction should succeed without panicking.
    }
}
