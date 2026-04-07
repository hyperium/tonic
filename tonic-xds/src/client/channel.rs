use crate::XdsUri;
use crate::client::cluster::ClusterClientRegistryGrpc;
use crate::client::endpoint::{EndpointAddress, EndpointChannel};
use crate::client::lb::{ClusterDiscovery, XdsLbService};
use crate::client::route::{Router, XdsRoutingLayer, XdsRoutingService};
use crate::common::async_util::BoxFuture;
use crate::xds::bootstrap::{BootstrapConfig, BootstrapError};
use crate::xds::cache::XdsCache;
use crate::xds::cluster_discovery::{
    EndpointConnector, XdsClusterDiscovery, default_endpoint_connector,
};
use crate::xds::resource_manager::XdsResourceManager;
use crate::xds::routing::XdsRouter;
use http::Request;
use std::fmt::Debug;
use std::sync::Arc;
use std::task::{Context, Poll};
use tonic::{body::Body as TonicBody, client::GrpcService, transport::channel::Channel};
use tower::{BoxError, Service, ServiceBuilder, load::Load, util::BoxCloneService};
use xds_client::{ClientConfig, Node, ProstCodec, TokioRuntime, TonicTransportBuilder, XdsClient};

use crate::client::retry::{GrpcRetryPolicy, GrpcRetryPolicyConfig, RetryLayer};

/// Configuration for building [`XdsChannel`] / [`XdsChannelGrpc`].
#[derive(Clone, Debug)]
pub struct XdsChannelConfig {
    target_uri: XdsUri,
    bootstrap: Option<BootstrapConfig>,
}

impl XdsChannelConfig {
    /// Creates a new config with the given target URI.
    #[must_use]
    pub fn new(target_uri: XdsUri) -> Self {
        Self {
            target_uri,
            bootstrap: None,
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

    /// Loads bootstrap configuration from environment variables and sets it.
    ///
    /// Reads from `GRPC_XDS_BOOTSTRAP` (file path) first, then falls back to
    /// `GRPC_XDS_BOOTSTRAP_CONFIG` (inline JSON).
    pub fn with_bootstrap_from_env(mut self) -> Result<Self, BootstrapError> {
        self.bootstrap = Some(BootstrapConfig::from_env()?);
        Ok(self)
    }
}

/// Errors that can occur when building an [`XdsChannel`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Bootstrap configuration could not be loaded.
    #[error("bootstrap: {0}")]
    Bootstrap(#[from] BootstrapError),
}

/// Holds owned resources whose background tasks must live as long as the channel.
///
/// When the last `XdsChannel` clone drops, this is dropped too, which aborts
/// the resource manager cascade task, the router watch task, and the ADS worker.
/// The `XdsCache` is kept alive by `XdsClusterDiscovery` in the service stack.
struct XdsChannelResources {
    _resource_manager: XdsResourceManager,
    _xds_client: XdsClient,
}

/// `XdsChannel` is an xDS-capable [`tower::Service`] implementation.
///
/// It routes requests according to the xDS configuration that it fetches from the xDS management server.
/// The routing implementation is based on the [Google gRPC xDS features](https://grpc.github.io/grpc/core/md_doc_grpc_xds_features.html).
///
/// # Type Parameters
///
/// * `Req` - The request type that this channel accepts, as an example: `http::Request<Body>`.
/// * `Endpoint` - The endpoint identifier type used for load balancing (e.g., socket address).
/// * `S` - The underlying [`tower::Service`] implementation that handles individual endpoint connections.
pub struct XdsChannel<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    config: Arc<XdsChannelConfig>,
    // Currently the routing decision is directly executed by the XdsLbService.
    // In the future, we will add more layers in between for retries, request mirroring, etc.
    inner: XdsRoutingService<XdsLbService<Req, Endpoint, S>>,
    /// Keeps background tasks alive. `None` when built from parts in tests.
    _resources: Option<Arc<XdsChannelResources>>,
}

#[allow(clippy::missing_fields_in_debug)]
impl<Req, Endpoint, S> Debug for XdsChannel<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdsChannel")
            .field("config", &self.config)
            .finish()
    }
}

impl<Req, Endpoint, S> Clone for XdsChannel<Req, Endpoint, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
    XdsRoutingService<XdsLbService<Req, Endpoint, S>>: Clone,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            inner: self.inner.clone(),
            _resources: self._resources.clone(),
        }
    }
}

impl<B, Endpoint, S> Service<http::Request<B>> for XdsChannel<Request<B>, Endpoint, S>
where
    B: Send + 'static,
    Request<B>: Send + 'static,
    Endpoint: std::hash::Hash + Eq + Clone + Send + 'static,
    S: Service<Request<B>> + Load + Send + 'static,
    S::Response: Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send,
    <S as tower::load::Load>::Metric: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        self.inner.call(request)
    }
}

/// A type alias for an `XdsChannel` that uses Tonic's Channel as the underlying transport.
pub(crate) type XdsChannelTonicGrpc =
    XdsChannel<http::Request<TonicBody>, EndpointAddress, EndpointChannel<Channel>>;

/// A [`tonic::client::GrpcService`] implementation that can route and load-balance
/// gRPC requests based on xDS configuration.
pub type XdsChannelGrpc =
    BoxCloneService<http::Request<TonicBody>, http::Response<TonicBody>, BoxError>;

// Static assertion that XdsChannelGrpc and XdsChannelTonicGrpc implement GrpcService
const _: fn() = || {
    fn assert_grpc_service<T: GrpcService<TonicBody>>() {}
    assert_grpc_service::<XdsChannelGrpc>();
    assert_grpc_service::<XdsChannelTonicGrpc>();
};

/// Builder for creating an [`XdsChannel`] or [`XdsChannelGrpc`].
#[derive(Clone, Debug)]
pub struct XdsChannelBuilder {
    config: Arc<XdsChannelConfig>,
}

impl XdsChannelBuilder {
    /// Creates a builder from a channel configuration.
    #[must_use]
    pub fn new(config: XdsChannelConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Builds an `XdsChannel`, which takes generic request, endpoint, and service types and can be
    /// used for generic HTTP services.
    pub fn build_channel<Req, Endpoint, S>(
        &self,
    ) -> Result<XdsChannel<Req, Endpoint, S>, BuildError>
    where
        Req: Send + 'static,
        S: Service<Req>,
        S::Response: Send + 'static,
    {
        todo!("Implement generic XdsChannel building logic");
    }

    fn build_tonic_grpc_channel(&self) -> Result<XdsChannelGrpc, BuildError> {
        let bootstrap = match self.config.bootstrap.clone() {
            Some(b) => b,
            None => BootstrapConfig::from_env()?,
        };

        let listener_name = self.config.target_uri.target.clone();

        let server_uri = bootstrap.server_uri().to_owned();
        let node = Node::from(bootstrap.node);
        let client_config = ClientConfig::new(node, server_uri);
        let xds_client = XdsClient::builder(
            client_config,
            TonicTransportBuilder::default(),
            ProstCodec,
            TokioRuntime,
        )
        .build();

        let cache = Arc::new(XdsCache::new());
        let resource_manager =
            XdsResourceManager::new(xds_client.clone(), cache.clone(), listener_name);

        Ok(self.build_from_cache(cache, xds_client, resource_manager))
    }

    /// Internal builder that wires the service stack from a pre-built cache.
    ///
    /// Separated from `build_tonic_grpc_channel` so tests can inject a
    /// disconnected `XdsClient` and pre-populated cache.
    fn build_from_cache(
        &self,
        cache: Arc<XdsCache>,
        xds_client: XdsClient,
        resource_manager: XdsResourceManager,
    ) -> XdsChannelGrpc {
        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));
        let connector: EndpointConnector = Arc::new(default_endpoint_connector);
        let discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>> =
            Arc::new(XdsClusterDiscovery::new(cache, connector));
        let retry_policy = GrpcRetryPolicy::new(GrpcRetryPolicyConfig::default());

        let _resources = Arc::new(XdsChannelResources {
            _resource_manager: resource_manager,
            _xds_client: xds_client,
        });

        let routing_layer = XdsRoutingLayer::new(router);
        let retry_layer = RetryLayer::new(retry_policy);
        let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
        let lb_service = XdsLbService::new(cluster_registry, discovery);
        let service = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);
        BoxCloneService::new(service)
    }

    /// Builds an `XdsChannelGrpc`, which is a type-erased gRPC channel.
    pub fn build_grpc_channel(&self) -> Result<XdsChannelGrpc, BuildError> {
        Ok(self.build_tonic_grpc_channel()?)
    }

    /// Builds an `XdsChannelGrpc` from the given router, cluster discovery, and retry policy.
    #[cfg(test)]
    pub(crate) fn build_grpc_channel_from_parts(
        &self,
        router: Arc<dyn Router>,
        discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>>,
        retry_policy: GrpcRetryPolicy,
    ) -> XdsChannelGrpc {
        let routing_layer = XdsRoutingLayer::new(router);
        let retry_layer = RetryLayer::new(retry_policy);
        let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
        let lb_service = XdsLbService::new(cluster_registry, discovery);
        let service = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(|req: Request<shared_http_body::SharedBody<TonicBody>>| {
                req.map(TonicBody::new)
            })
            .service(lb_service);
        BoxCloneService::new(service)
    }
}

#[cfg(test)]
mod tests {
    use super::{XdsChannelBuilder, XdsChannelConfig};
    use crate::XdsUri;
    use crate::client::channel::XdsChannelGrpc;
    use crate::client::endpoint::EndpointAddress;
    use crate::client::endpoint::EndpointChannel;

    fn test_config() -> XdsChannelConfig {
        XdsChannelConfig::new(XdsUri::parse("xds:///test-service").unwrap())
    }
    use crate::client::lb::{BoxDiscover, ClusterDiscovery};
    use crate::client::retry::GrpcRetryPolicy;
    use crate::client::route::RouteDecision;
    use crate::client::route::RouteInput;
    use crate::client::route::Router;
    use crate::common::async_util::BoxFuture;
    use crate::testutil::grpc::GreeterClient;
    use crate::testutil::grpc::HelloRequest;
    use crate::testutil::grpc::TestServer;
    use crate::xds::cache::XdsCache;
    use crate::xds::resource::EndpointsResource;
    use crate::xds::resource::route_config::RouteConfigResource;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tonic::transport::Channel;
    use tower::discover::Change;

    /// Sets up multiple gRPC test servers and returns their addresses, clients and shutdown handles.
    async fn setup_grpc_servers(
        count: usize,
    ) -> (Vec<String>, Vec<crate::testutil::grpc::TestServer>) {
        use crate::testutil::grpc::spawn_greeter_server;

        let mut servers = Vec::new();
        let mut server_addrs = Vec::new();

        for i in 0..count {
            let server_name = format!("server-{i}");
            let server = spawn_greeter_server(&server_name, None, None)
                .await
                .expect("Failed to spawn gRPC server");

            server_addrs.push(server.addr.to_string());
            servers.push(server);
        }

        (server_addrs, servers)
    }

    /// A mock XdsManager that provides pre-configured endpoints for testing.
    struct MockXdsManager {
        endpoints: Vec<(EndpointAddress, Channel)>,
    }

    impl MockXdsManager {
        /// Creates a new MockXdsManager from test servers.
        fn from_test_servers(servers: &[TestServer]) -> Self {
            let endpoints = servers
                .iter()
                .map(|s| {
                    let addr = EndpointAddress::from(s.addr);
                    (addr, s.channel.clone())
                })
                .collect();
            Self { endpoints }
        }
    }

    impl Router for MockXdsManager {
        fn route(
            &self,
            _input: &RouteInput<'_>,
        ) -> BoxFuture<Result<RouteDecision, crate::xds::routing::RoutingError>> {
            Box::pin(async move {
                Ok(RouteDecision {
                    cluster: "test-cluster".to_string(),
                })
            })
        }
    }

    impl ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>> for MockXdsManager {
        fn discover_cluster(
            &self,
            _cluster_name: &str,
        ) -> BoxDiscover<EndpointAddress, EndpointChannel<Channel>> {
            let endpoints = self.endpoints.clone();
            let (tx, rx) = mpsc::channel(16);

            tokio::spawn(async move {
                for (addr, channel) in endpoints {
                    let endpoint_channel = EndpointChannel::new(channel);
                    let change = Change::Insert(addr, endpoint_channel);
                    tx.send(Ok(change)).await.expect("Failed to send SD change");
                }
            });

            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
        }
    }

    /// Sends multiple gRPC requests using the provided client and returns statistics about the requests.
    async fn send_grpc_requests(
        mut grpc_client: crate::testutil::grpc::GreeterClient<XdsChannelGrpc>,
        num_requests: usize,
    ) -> (
        usize,
        std::collections::HashMap<String, usize>,
        std::collections::HashMap<String, usize>,
    ) {
        let mut successful_requests = 0;
        let mut error_types = std::collections::HashMap::new();
        let mut server_counts = std::collections::HashMap::new();

        for i in 0..num_requests {
            let request_timeout = tokio::time::Duration::from_secs(3);
            let request_future = grpc_client.say_hello(HelloRequest {
                name: format!("test-request-{i}"),
            });

            match tokio::time::timeout(request_timeout, request_future).await {
                Ok(Ok(response)) => {
                    successful_requests += 1;
                    // Extract server name from response message (format: "server-X: test-request-Y")
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

    #[tokio::test]
    /// Tests the `XdsChannelGrpc` with a power-of-two-choices load balancer.
    async fn test_xds_channel_grpc_with_p2c_lb() {
        let num_requests = 1000;
        let num_servers = 5;
        let (_, servers) = setup_grpc_servers(num_servers).await;

        // Create a mock XdsManager with the test servers
        let xds_manager = Arc::new(MockXdsManager::from_test_servers(&servers));

        let xds_channel_builder = XdsChannelBuilder::new(test_config());
        let xds_channel = xds_channel_builder.build_grpc_channel_from_parts(
            xds_manager.clone(),
            xds_manager.clone(),
            GrpcRetryPolicy::default(),
        );

        let client = GreeterClient::new(xds_channel);

        let (successful_requests, error_types, server_counts) =
            send_grpc_requests(client, num_requests).await;

        println!("Successful requests: {successful_requests}");
        println!("Error types: {error_types:?}");
        println!("Per-server call counts: {server_counts:?}");

        assert_eq!(
            successful_requests, num_requests,
            "Expected 100% success rate. Got {successful_requests} successful out of {num_requests} requests. Errors: {error_types:?}",
        );

        assert!(
            error_types.is_empty(),
            "Expected no errors but got: {error_types:?}",
        );

        let actual_server_count = server_counts.len();
        assert_eq!(
            actual_server_count, num_servers,
            "Expected all {num_servers} servers to receive requests, but only {actual_server_count} servers received traffic. Server counts: {server_counts:?}",
        );

        let expected_per_server = num_requests / num_servers;
        let min_requests_per_server = (expected_per_server as f64 / 1.5) as usize;
        let max_requests_per_server = (expected_per_server as f64 * 1.5) as usize;

        for (server_name, count) in &server_counts {
            assert!(
                *count >= min_requests_per_server,
                "Server {server_name} received only {count} requests, expected at least {min_requests_per_server} (expected ~{expected_per_server} per server with 1.5x variance)",
            );
            assert!(
                *count <= max_requests_per_server,
                "Server {server_name} received {count} requests, expected at most {max_requests_per_server} (expected ~{expected_per_server} per server with 1.5x variance)",
            );
        }

        let total_server_requests: usize = server_counts.values().sum();
        assert_eq!(
            total_server_requests, successful_requests,
            "Total server requests ({total_server_requests}) should equal successful requests ({successful_requests}). Server counts: {server_counts:?}",
        );

        for server in servers {
            let _ = server.shutdown.send(());
            let _ = server.handle.await;
        }
    }

    #[tokio::test]
    async fn test_retry_once_on_unavailable() {
        use crate::client::retry::{GrpcRetryPolicy, GrpcRetryPolicyConfig};
        use crate::testutil::grpc::spawn_fail_first_n_server;

        // Server fails the first request with UNAVAILABLE, succeeds on retry.
        let server = spawn_fail_first_n_server("retry-server", 1)
            .await
            .expect("Failed to spawn server");

        let servers = vec![server];
        let xds_manager = Arc::new(MockXdsManager::from_test_servers(&servers));

        let retry_policy = GrpcRetryPolicy::new(
            GrpcRetryPolicyConfig::new()
                .retry_on(vec![tonic::Code::Unavailable])
                .num_retries(1),
        );

        let xds_channel = XdsChannelBuilder::new(test_config())
            .build_grpc_channel_from_parts(xds_manager.clone(), xds_manager.clone(), retry_policy);

        let mut client = GreeterClient::new(xds_channel);

        let response = client
            .say_hello(HelloRequest {
                name: "retry-test".to_string(),
            })
            .await
            .expect("request should succeed after retry");

        assert_eq!(response.into_inner().message, "retry-server: retry-test");
    }

    /// Helper: creates a `RouteConfigResource` that routes all traffic to the given cluster.
    fn make_test_route_config(cluster_name: &str) -> Arc<RouteConfigResource> {
        use crate::xds::resource::route_config::*;

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
        })
    }

    /// Helper: creates an `EndpointsResource` from test server addresses.
    fn make_test_endpoints(cluster_name: &str, servers: &[TestServer]) -> Arc<EndpointsResource> {
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

    /// Builds an XdsChannelGrpc using real XdsRouter and XdsClusterDiscovery
    /// backed by the given cache. Yields once so the router's background watch
    /// task processes the initial snapshot (the task is spawned during
    /// `XdsRouter::new` and needs one poll to read from the cache watch).
    async fn build_xds_channel_from_cache(cache: Arc<XdsCache>) -> XdsChannelGrpc {
        use crate::xds::cluster_discovery::{
            EndpointConnector, XdsClusterDiscovery, default_endpoint_connector,
        };
        use crate::xds::routing::XdsRouter;

        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));
        let connector: EndpointConnector = Arc::new(default_endpoint_connector);
        let discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>> =
            Arc::new(XdsClusterDiscovery::new(cache, connector));

        tokio::task::yield_now().await;

        let builder = XdsChannelBuilder::new(test_config());
        builder.build_grpc_channel_from_parts(router, discovery, GrpcRetryPolicy::default())
    }

    /// Tests the full xDS stack (XdsRouter + XdsClusterDiscovery) with a
    /// pre-populated cache, validating that requests are routed and
    /// load-balanced across real backend servers.
    #[tokio::test]
    async fn test_xds_channel_with_real_router_and_discovery() {
        let num_servers = 3;
        let num_requests = 300;
        let cluster_name = "test-cluster";
        let (_, servers) = setup_grpc_servers(num_servers).await;

        let cache = Arc::new(XdsCache::new());
        cache.update_route_config(make_test_route_config(cluster_name));
        cache.update_endpoints(cluster_name, make_test_endpoints(cluster_name, &servers));

        let channel = build_xds_channel_from_cache(cache).await;
        let client = GreeterClient::new(channel);

        let (successful, error_types, server_counts) =
            send_grpc_requests(client, num_requests).await;

        assert_eq!(
            successful, num_requests,
            "Expected 100% success rate. Errors: {error_types:?}",
        );
        assert_eq!(
            server_counts.len(),
            num_servers,
            "Expected all {num_servers} servers to receive traffic. Counts: {server_counts:?}",
        );

        for server in servers {
            let _ = server.shutdown.send(());
            let _ = server.handle.await;
        }
    }

    /// Tests that endpoint changes are picked up dynamically by the
    /// XdsClusterDiscovery while the channel is serving requests.
    #[tokio::test]
    async fn test_xds_channel_handles_dynamic_endpoint_updates() {
        let cluster_name = "test-cluster";
        let (_, servers) = setup_grpc_servers(2).await;

        let cache = Arc::new(XdsCache::new());
        cache.update_route_config(make_test_route_config(cluster_name));
        // Start with only the first server.
        cache.update_endpoints(
            cluster_name,
            make_test_endpoints(cluster_name, &servers[..1]),
        );

        let channel = build_xds_channel_from_cache(cache.clone()).await;
        let client = GreeterClient::new(channel.clone());

        // Phase 1: all traffic goes to server-0.
        let (successful, _, server_counts) = send_grpc_requests(client, 50).await;
        assert_eq!(successful, 50);
        assert_eq!(
            server_counts.len(),
            1,
            "Only 1 server should receive traffic before update. Counts: {server_counts:?}",
        );

        // Add second server.
        cache.update_endpoints(cluster_name, make_test_endpoints(cluster_name, &servers));
        // Give the endpoint manager diff loop time to process the update.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Phase 2: traffic should go to both servers.
        let client2 = GreeterClient::new(channel);
        let (successful, _, server_counts) = send_grpc_requests(client2, 200).await;
        assert_eq!(successful, 200);
        assert_eq!(
            server_counts.len(),
            2,
            "Both servers should receive traffic after update. Counts: {server_counts:?}",
        );

        for server in servers {
            let _ = server.shutdown.send(());
            let _ = server.handle.await;
        }
    }

    /// Smoke test: verifies builder wiring with a disconnected XdsClient
    /// doesn't panic during construction.
    #[tokio::test]
    async fn test_build_from_cache_smoke() {
        use crate::xds::resource_manager::XdsResourceManager;

        let cache = Arc::new(XdsCache::new());
        let xds_client = xds_client::XdsClient::disconnected();
        let resource_manager =
            XdsResourceManager::new(xds_client.clone(), cache.clone(), "test-listener".into());

        let builder = XdsChannelBuilder::new(test_config());
        let _channel = builder.build_from_cache(cache, xds_client, resource_manager);
        // Construction should succeed without panicking.
    }
}
