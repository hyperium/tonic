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

//! xDS config dump through the Client Status Discovery Service (gRFC A40).
//!
//! Every xDS channel registers its xDS client in a process-wide registry when
//! it is built, and removes it when the last clone of the channel is dropped.
//! [`CsdsService`] reports the resources of every registered client.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use envoy_types::pb::envoy::admin::v3::{ClientResourceStatus, UpdateFailureState};
use envoy_types::pb::envoy::config::core::v3::Node as NodeProto;
use envoy_types::pb::envoy::service::status::v3::client_config::GenericXdsConfig;
use envoy_types::pb::envoy::service::status::v3::client_status_discovery_service_server::{
    ClientStatusDiscoveryService, ClientStatusDiscoveryServiceServer,
};
use envoy_types::pb::envoy::service::status::v3::{
    ClientConfig, ClientStatusRequest, ClientStatusResponse,
};
use envoy_types::pb::google::protobuf::{Any, Timestamp};
use futures_core::Stream;
use futures_util::StreamExt;
use prost::Message;
use tonic::server::NamedService;
use tonic::{Request, Response, Status, Streaming};
use tower::{BoxError, Service};
use xds_client::{Node, ProstCodec, ResourceSnapshot, ResourceStatus, XdsClient};

static GLOBAL_REGISTRY: LazyLock<Arc<CsdsRegistry>> = LazyLock::new(Default::default);

/// The xDS clients that a [`CsdsService`] reports on.
#[derive(Default)]
pub(crate) struct CsdsRegistry {
    next_id: AtomicU64,
    clients: Mutex<BTreeMap<u64, RegisteredClient>>,
}

#[derive(Clone)]
struct RegisteredClient {
    /// The channel target that the client serves (gRFC A71 `client_scope`).
    scope: String,
    node: NodeProto,
    client: XdsClient,
}

impl CsdsRegistry {
    /// The registry that every xDS channel registers in.
    pub(crate) fn global() -> Arc<Self> {
        Arc::clone(&GLOBAL_REGISTRY)
    }

    /// Registers `client`, which serves the channel target `scope` and
    /// identifies itself to its xDS server as `node`. The client stays
    /// registered until the returned registration is dropped.
    pub(crate) fn register(
        self: &Arc<Self>,
        scope: String,
        node: &Node,
        client: XdsClient,
    ) -> CsdsRegistration {
        // The node is reported exactly as ADS requests send it. Decoding what
        // the same codec just encoded cannot fail.
        let node = NodeProto::decode(ProstCodec.encode_node(node)).unwrap_or_default();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(
            id,
            RegisteredClient {
                scope,
                node,
                client,
            },
        );
        CsdsRegistration {
            registry: Arc::clone(self),
            id,
        }
    }

    /// The registered clients, in registration order.
    fn clients(&self) -> Vec<RegisteredClient> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, RegisteredClient>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Keeps an xDS client registered with a [`CsdsRegistry`] until dropped.
pub(crate) struct CsdsRegistration {
    registry: Arc<CsdsRegistry>,
    id: u64,
}

impl Drop for CsdsRegistration {
    fn drop(&mut self) {
        self.registry.lock().remove(&self.id);
    }
}

/// A Client Status Discovery Service (CSDS, gRFC A40) that reports the xDS
/// configuration of every xDS channel in the process.
///
/// Each channel has its own xDS client, reported as one `ClientConfig` whose
/// `client_scope` is the channel's target. For each resource the client
/// subscribes to, the dump reports its status, the last version the client
/// accepted as the xDS server sent it, and the most recent update it rejected.
///
/// The `REQUESTED`, `DOES_NOT_EXIST`, `ACKED`, and `NACKED` statuses are
/// reported. `RECEIVED_ERROR` and `TIMEOUT` (gRFC A88) are not, because the xDS
/// client does not yet handle errors sent by the xDS server or treat the
/// resource timer as a transient failure. Requests that set `node_matchers` are
/// rejected with `INVALID_ARGUMENT`. Requests that set
/// `exclude_resource_contents` get every field except the resources themselves.
///
/// ```no_run
/// # async fn serve() -> Result<(), Box<dyn std::error::Error>> {
/// tonic::transport::Server::builder()
///     .add_service(tonic_xds::CsdsService::new())
///     .serve("[::1]:50051".parse()?)
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct CsdsService {
    inner: ClientStatusDiscoveryServiceServer<CsdsHandler>,
}

impl CsdsService {
    /// Creates a service that reports on every xDS channel in the process.
    pub fn new() -> Self {
        Self::with_handler(CsdsHandler {
            registry: CsdsRegistry::global(),
        })
    }

    /// Creates a service that reports on the clients in `registry`.
    #[cfg(test)]
    pub(crate) fn with_registry(registry: Arc<CsdsRegistry>) -> Self {
        Self::with_handler(CsdsHandler { registry })
    }

    fn with_handler(handler: CsdsHandler) -> Self {
        Self {
            inner: ClientStatusDiscoveryServiceServer::new(handler),
        }
    }
}

impl Default for CsdsService {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CsdsService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CsdsService").finish_non_exhaustive()
    }
}

impl NamedService for CsdsService {
    const NAME: &'static str =
        <ClientStatusDiscoveryServiceServer<CsdsHandler> as NamedService>::NAME;
}

impl<B> Service<http::Request<B>> for CsdsService
where
    B: http_body::Body + Send + 'static,
    B::Error: Into<BoxError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Service::<http::Request<B>>::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        self.inner.call(request)
    }
}

#[derive(Clone)]
struct CsdsHandler {
    registry: Arc<CsdsRegistry>,
}

impl CsdsHandler {
    async fn client_status(
        &self,
        request: ClientStatusRequest,
    ) -> Result<ClientStatusResponse, Status> {
        if !request.node_matchers.is_empty() {
            return Err(Status::invalid_argument("node_matchers not supported"));
        }
        let include_contents = !request.exclude_resource_contents;
        let configs = self
            .registry
            .clients()
            .into_iter()
            .map(|client| self.client_config(client, include_contents));
        let config = futures_util::future::join_all(configs).await;
        Ok(ClientStatusResponse { config })
    }

    async fn client_config(
        &self,
        registered: RegisteredClient,
        include_contents: bool,
    ) -> ClientConfig {
        let snapshot = registered.client.resource_snapshot().await;
        ClientConfig {
            node: Some(registered.node),
            generic_xds_configs: snapshot
                .into_iter()
                .map(|resource| generic_xds_config(resource, include_contents))
                .collect(),
            client_scope: registered.scope,
            ..Default::default()
        }
    }
}

#[tonic::async_trait]
impl ClientStatusDiscoveryService for CsdsHandler {
    type StreamClientStatusStream =
        Pin<Box<dyn Stream<Item = Result<ClientStatusResponse, Status>> + Send>>;

    async fn stream_client_status(
        &self,
        request: Request<Streaming<ClientStatusRequest>>,
    ) -> Result<Response<Self::StreamClientStatusStream>, Status> {
        let handler = self.clone();
        let responses = request.into_inner().then(move |request| {
            let handler = handler.clone();
            async move { handler.client_status(request?).await }
        });
        Ok(Response::new(Box::pin(responses)))
    }

    async fn fetch_client_status(
        &self,
        request: Request<ClientStatusRequest>,
    ) -> Result<Response<ClientStatusResponse>, Status> {
        self.client_status(request.into_inner())
            .await
            .map(Response::new)
    }
}

/// Converts `resource` to its CSDS form. The resource itself is left out
/// unless `include_contents` is set.
fn generic_xds_config(resource: ResourceSnapshot, include_contents: bool) -> GenericXdsConfig {
    let mut config = GenericXdsConfig {
        type_url: resource.type_url,
        name: resource.name,
        client_status: client_status(resource.status) as i32,
        ..Default::default()
    };
    if let Some(accepted) = resource.accepted {
        config.version_info = accepted.version_info;
        config.last_updated = Some(timestamp(accepted.last_updated));
        if include_contents {
            config.xds_config = Some(Any {
                type_url: accepted.resource.type_url,
                value: accepted.resource.value.to_vec(),
            });
        }
    }
    if let Some(rejected) = resource.rejected {
        config.error_state = Some(UpdateFailureState {
            failed_configuration: None,
            last_update_attempt: Some(timestamp(rejected.last_update_attempt)),
            details: rejected.details,
            version_info: rejected.version_info,
        });
    }
    config
}

fn client_status(status: ResourceStatus) -> ClientResourceStatus {
    match status {
        ResourceStatus::Requested => ClientResourceStatus::Requested,
        ResourceStatus::DoesNotExist => ClientResourceStatus::DoesNotExist,
        ResourceStatus::Acked => ClientResourceStatus::Acked,
        ResourceStatus::Nacked => ClientResourceStatus::Nacked,
        _ => ClientResourceStatus::Unknown,
    }
}

fn timestamp(time: SystemTime) -> Timestamp {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX),
        nanos: since_epoch.subsec_nanos() as i32,
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use bytes::Bytes;
    use envoy_types::pb::envoy::service::status::v3::client_status_discovery_service_client::ClientStatusDiscoveryServiceClient;
    use envoy_types::pb::envoy::r#type::matcher::v3::NodeMatcher;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use xds_client::{
        AcceptedResource, ClientConfig as XdsClientConfig, RejectedUpdate, ResourceAny,
        ServerConfig, TokioRuntime, TonicTransport, TransportBuilder,
    };

    use super::*;
    use crate::xds::resource::ListenerResource;

    const LISTENER_TYPE_URL: &str = "type.googleapis.com/envoy.config.listener.v3.Listener";

    fn node(id: &str) -> Node {
        Node::new("grpc", "1.0").with_id(id)
    }

    fn handler(registry: &Arc<CsdsRegistry>) -> CsdsHandler {
        CsdsHandler {
            registry: Arc::clone(registry),
        }
    }

    /// `nanos` should be a multiple of 100, the resolution of `SystemTime` on
    /// Windows.
    fn time(seconds: u64, nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(seconds, nanos)
    }

    fn nacked_listener() -> ResourceSnapshot {
        ResourceSnapshot::new(LISTENER_TYPE_URL, "listener", ResourceStatus::Nacked)
            .with_accepted(AcceptedResource::new(
                "1",
                ResourceAny {
                    type_url: LISTENER_TYPE_URL.to_string(),
                    value: Bytes::from_static(b"listener bytes"),
                },
                time(100, 500),
            ))
            .with_rejected(RejectedUpdate::new("2", "invalid listener", time(200, 700)))
    }

    #[test]
    fn rejected_resource_reports_accepted_and_rejected_versions() {
        assert_eq!(
            generic_xds_config(nacked_listener(), true),
            GenericXdsConfig {
                type_url: LISTENER_TYPE_URL.to_string(),
                name: "listener".to_string(),
                version_info: "1".to_string(),
                xds_config: Some(Any {
                    type_url: LISTENER_TYPE_URL.to_string(),
                    value: b"listener bytes".to_vec(),
                }),
                last_updated: Some(Timestamp {
                    seconds: 100,
                    nanos: 500,
                }),
                client_status: ClientResourceStatus::Nacked as i32,
                error_state: Some(UpdateFailureState {
                    failed_configuration: None,
                    last_update_attempt: Some(Timestamp {
                        seconds: 200,
                        nanos: 700,
                    }),
                    details: "invalid listener".to_string(),
                    version_info: "2".to_string(),
                }),
                ..Default::default()
            }
        );
    }

    #[test]
    fn excluding_contents_omits_only_the_resource() {
        let with_contents = generic_xds_config(nacked_listener(), true);
        let without_contents = generic_xds_config(nacked_listener(), false);
        assert!(without_contents.xds_config.is_none());
        assert_eq!(
            without_contents,
            GenericXdsConfig {
                xds_config: None,
                ..with_contents
            }
        );
    }

    #[test]
    fn requested_resource_has_no_config() {
        let config = generic_xds_config(
            ResourceSnapshot::new(LISTENER_TYPE_URL, "listener", ResourceStatus::Requested),
            true,
        );

        assert_eq!(
            config,
            GenericXdsConfig {
                type_url: LISTENER_TYPE_URL.to_string(),
                name: "listener".to_string(),
                client_status: ClientResourceStatus::Requested as i32,
                ..Default::default()
            }
        );
    }

    #[test]
    fn statuses_map_to_csds_statuses() {
        assert_eq!(
            client_status(ResourceStatus::Requested),
            ClientResourceStatus::Requested
        );
        assert_eq!(
            client_status(ResourceStatus::DoesNotExist),
            ClientResourceStatus::DoesNotExist
        );
        assert_eq!(
            client_status(ResourceStatus::Acked),
            ClientResourceStatus::Acked
        );
        assert_eq!(
            client_status(ResourceStatus::Nacked),
            ClientResourceStatus::Nacked
        );
    }

    #[tokio::test]
    async fn reports_each_registered_client_until_unregistered() {
        let registry = Arc::new(CsdsRegistry::default());
        let _a = registry.register(
            "xds:///a".to_string(),
            &node("a"),
            XdsClient::disconnected(),
        );
        let b = registry.register(
            "xds:///b".to_string(),
            &node("b"),
            XdsClient::disconnected(),
        );

        let response = handler(&registry)
            .client_status(ClientStatusRequest::default())
            .await
            .unwrap();
        let clients: Vec<(&str, &str)> = response
            .config
            .iter()
            .map(|c| {
                (
                    c.client_scope.as_str(),
                    c.node.as_ref().unwrap().id.as_str(),
                )
            })
            .collect();
        assert_eq!(clients, [("xds:///a", "a"), ("xds:///b", "b")]);
        let reported = response.config[0].node.clone().unwrap();
        assert_eq!(
            reported,
            NodeProto::decode(ProstCodec.encode_node(&node("a"))).unwrap()
        );

        drop(b);
        let response = handler(&registry)
            .client_status(ClientStatusRequest::default())
            .await
            .unwrap();
        assert_eq!(response.config.len(), 1);
        assert_eq!(response.config[0].client_scope, "xds:///a");
    }

    #[tokio::test]
    async fn rejects_node_matchers() {
        let request = ClientStatusRequest {
            node_matchers: vec![NodeMatcher::default()],
            ..Default::default()
        };
        let status = handler(&Arc::default())
            .client_status(request)
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    /// Never connects, so the client stays connecting.
    struct PendingTransportBuilder;

    impl TransportBuilder for PendingTransportBuilder {
        type Transport = TonicTransport;

        fn build(
            &self,
            _server: &ServerConfig,
        ) -> impl Future<Output = xds_client::Result<TonicTransport>> + Send {
            std::future::pending()
        }
    }

    #[tokio::test]
    async fn reports_a_client_that_is_still_connecting() {
        let config = XdsClientConfig::new(node("stuck"), "http://xds.example.com");
        let client =
            XdsClient::builder(config, PendingTransportBuilder, ProstCodec, TokioRuntime).build();
        // Subscribing makes the worker start connecting.
        let _watcher = client.watch::<ListenerResource>("listener").await;
        let registry = Arc::new(CsdsRegistry::default());
        let _registration = registry.register("xds:///stuck".to_string(), &node("stuck"), client);

        let response = handler(&registry)
            .client_status(ClientStatusRequest::default())
            .await
            .unwrap();
        assert_eq!(response.config.len(), 1);
        assert_eq!(response.config[0].client_scope, "xds:///stuck");
        assert_eq!(
            response.config[0].generic_xds_configs,
            [GenericXdsConfig {
                type_url: LISTENER_TYPE_URL.to_string(),
                name: "listener".to_string(),
                client_status: ClientResourceStatus::Requested as i32,
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn serves_fetch_and_stream() {
        let registry = Arc::new(CsdsRegistry::default());
        let _registration = registry.register(
            "xds:///a".to_string(),
            &node("a"),
            XdsClient::disconnected(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let service = CsdsService::with_handler(handler(&registry));
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let mut client = ClientStatusDiscoveryServiceClient::connect(format!("http://{addr}"))
            .await
            .unwrap();

        let response = client
            .fetch_client_status(ClientStatusRequest::default())
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.config[0].client_scope, "xds:///a");

        // One response per request.
        let requests = tokio_stream::iter(vec![
            ClientStatusRequest::default(),
            ClientStatusRequest::default(),
        ]);
        let mut responses = client
            .stream_client_status(requests)
            .await
            .unwrap()
            .into_inner();
        for _ in 0..2 {
            let response = responses.message().await.unwrap().unwrap();
            assert_eq!(response.config[0].client_scope, "xds:///a");
        }
        assert!(responses.message().await.unwrap().is_none());
    }
}
