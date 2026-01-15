use crate::client::endpoint::{EndpointAddress, EndpointChannel};
use crate::client::lb::XdsLbService;
use crate::client::route::XdsRoutingService;
use crate::XdsUri;
use http::Request;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tonic::{body::Body as TonicBody, client::GrpcService, transport::channel::Channel};
use tower::{load::Load, util::BoxCloneService, BoxError, Service};

#[cfg(test)]
use {
    crate::client::cluster::ClusterClientRegistryGrpc, crate::client::route::XdsRoutingLayer,
    crate::xds::xds_manager::XdsManager, tower::ServiceBuilder,
};

/// Configuration for an xDS-capable channel.
/// Currently, only support specifying the xDS URI for the target service.
/// In the future, more configurations such as xDS management server address will be added.
#[derive(Clone, Debug, Default)]
pub struct XdsChannelConfig {
    target_uri: Option<XdsUri>,
}

impl XdsChannelConfig {
    /// Sets the xDS URI for the channel.
    #[must_use]
    pub fn with_target_uri(mut self, target_uri: XdsUri) -> Self {
        self.target_uri = Some(target_uri);
        self
    }
}

/// `XdsChannel` is an xDS-capable Tower Service.
///
/// It routes requests according to the xDS configuration that it fetches from the xDS management server.
/// The routing implementation is based on the [Google gRPC xDS features](https://grpc.github.io/grpc/core/md_doc_grpc_xds_features.html).
///
/// # Type Parameters
///
/// * `Req` - The request type that this channel accepts, as an example: `http::Request<Body>`.
/// * `E` - The endpoint identifier type used for load balancing (e.g., socket address).
/// * `S` - The underlying Tower Service type that handles individual endpoint connections.
pub struct XdsChannel<Req, E, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
{
    config: Arc<XdsChannelConfig>,
    // Currently the routing decision is directly executed by the XdsLbService.
    // In the future, we will add more layers in between for retries, request mirroring, etc.
    inner: XdsRoutingService<XdsLbService<Req, E, S>>,
}

#[allow(clippy::missing_fields_in_debug)]
impl<Req, E, S> Debug for XdsChannel<Req, E, S>
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

impl<Req, E, S> Clone for XdsChannel<Req, E, S>
where
    Req: Send + 'static,
    S: Service<Req>,
    S::Response: Send + 'static,
    XdsRoutingService<XdsLbService<Req, E, S>>: Clone,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            inner: self.inner.clone(),
        }
    }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<B, E, S> Service<http::Request<B>> for XdsChannel<Request<B>, E, S>
where
    B: Send + 'static,
    Request<B>: Send + 'static,
    E: std::hash::Hash + Eq + Clone + Send + 'static,
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

/// A type-erased gRPC channel.
pub type XdsChannelGrpc =
    BoxCloneService<http::Request<TonicBody>, http::Response<TonicBody>, BoxError>;

// Static assertion that XdsChannelGrpc and XdsChannelTonicGrpc implement GrpcService
const _: fn() = || {
    fn assert_grpc_service<T: GrpcService<TonicBody>>() {}
    assert_grpc_service::<XdsChannelGrpc>();
    assert_grpc_service::<XdsChannelTonicGrpc>();
};

/// Builder for creating an `XdsChannel` or `XdsChannelGrpc`.
#[derive(Clone, Debug)]
pub struct XdsChannelBuilder {
    #[allow(dead_code)]
    config: Arc<XdsChannelConfig>,
}

impl XdsChannelBuilder {
    /// Create a builder from an channel configurations.
    #[must_use]
    pub fn with_config(config: XdsChannelConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Builds an `XdsChannel`, which takes generic request, endpoint, and service types and can be
    /// used for generic HTTP services.
    #[must_use]
    pub fn build_channel<Req, E, S>(&self) -> XdsChannel<Req, E, S>
    where
        Req: Send + 'static,
        S: Service<Req>,
        S::Response: Send + 'static,
    {
        todo!("Implement XdsChannel building logic");
    }

    pub(crate) fn build_tonic_grpc_channel(&self) -> XdsChannelTonicGrpc {
        todo!("Implement XdsChannel building logic");
    }

    /// Builds an `XdsChannelGrpc`, which is a type-erased gRPC channel.
    #[must_use]
    pub fn build_grpc_channel(&self) -> XdsChannelGrpc {
        BoxCloneService::new(self.build_tonic_grpc_channel())
    }

    /// Builds an `XdsChannelGrpc` from the given xDS manager dependencies.
    /// This is primarily intended for testing purposes for now.
    /// [`XdsChannelBuilder::build_grpc_channel`] should build [`XdsManager`](crate::xds::xds_manager::XdsManager)
    /// as part of constructing `XdsChannelGrpc`.
    #[cfg(test)]
    pub(crate) fn build_grpc_channel_from_xds_manager(
        &self,
        xds_manager: Arc<dyn XdsManager<EndpointAddress, EndpointChannel<Channel>>>,
    ) -> XdsChannelGrpc {
        let routing_layer = XdsRoutingLayer::new(xds_manager.clone());
        let cluster_registry = Arc::new(ClusterClientRegistryGrpc::new());
        let lb_service = XdsLbService::new(cluster_registry, xds_manager.clone());
        let service = ServiceBuilder::new()
            .layer(routing_layer)
            .service(lb_service);
        BoxCloneService::new(XdsChannelTonicGrpc {
            config: self.config.clone(),
            inner: service,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::XdsChannelBuilder;
    use super::XdsChannelConfig;
    use crate::client::channel::XdsChannelGrpc;
    use crate::client::endpoint::EndpointAddress;
    use crate::client::endpoint::EndpointChannel;
    use crate::testutil::grpc::GreeterClient;
    use crate::testutil::grpc::HelloRequest;
    use crate::testutil::grpc::TestServer;
    use crate::xds::route::RouteDecision;
    use crate::xds::route::RouteInput;
    use crate::xds::xds_manager::{BoxDiscover, BoxFut};
    use crate::xds::xds_manager::{XdsClusterDiscovery, XdsRouter};
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

    impl XdsRouter for MockXdsManager {
        fn route(&self, _input: &RouteInput<'_>) -> BoxFut<RouteDecision> {
            Box::pin(async move {
                RouteDecision {
                    cluster: "test-cluster".to_string(),
                }
            })
        }
    }

    impl XdsClusterDiscovery<EndpointAddress, EndpointChannel<Channel>> for MockXdsManager {
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

        let xds_channel_builder = XdsChannelBuilder::with_config(XdsChannelConfig::default());
        let xds_channel =
            xds_channel_builder.build_grpc_channel_from_xds_manager(xds_manager.clone());

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
}
