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

//! Per-RPC configuration selection.
//!
//! A name resolver may attach a [`ConfigSelector`] to a [`ResolverUpdate`] to
//! control the configuration applied to each RPC made on the channel.

use std::fmt::Debug;
use std::sync::Arc;

use crate::StatusError;
use crate::client::load_balancing::PickOptions;
use crate::client::name_resolution::ResolverUpdate;

/// The configuration to apply to a single RPC, as chosen by a
/// [`ConfigSelector`].
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RpcConfig {}

/// Controls what configuration to use for every RPC.
///
/// A `ConfigSelector` is supplied by a name resolver through the attributes of
/// a [`ResolverUpdate`] and is consulted by the channel once per RPC.
pub trait ConfigSelector: Send + Sync + Debug {
    /// Selects the configuration for the RPC, or terminates it with the
    /// returned error.
    ///
    /// Errors with a status code that the control plane may not use, as
    /// defined by gRFC A54, are converted to INTERNAL by the channel.
    fn select_config(&self, options: PickOptions) -> Result<RpcConfig, StatusError>;
}

/// The config selector a channel uses when its resolver does not supply one.
#[derive(Debug)]
pub(crate) struct DefaultConfigSelector;

impl ConfigSelector for DefaultConfigSelector {
    fn select_config(&self, _options: PickOptions) -> Result<RpcConfig, StatusError> {
        Ok(RpcConfig::default())
    }
}

/// Returns the `ConfigSelector` stored in the attributes of `update`, if
/// any. Returns the [`ResolverUpdate`] with the [`ConfigSelector`] removed.
pub(crate) fn from_resolver_update(
    mut update: ResolverUpdate,
) -> (Option<Arc<dyn ConfigSelector>>, ResolverUpdate) {
    if let Some(cs) = update
        .attributes
        .get::<ConfigSelectorAttr>()
        .map(|attr| attr.0.clone())
    {
        update.attributes = update.attributes.remove::<ConfigSelectorAttr>();
        return (Some(cs), update);
    }
    (None, update)
}

/// Stores this `ConfigSelector` in the attributes of `update`, replacing
/// any previously stored selector, and returns the updated
/// `ResolverUpdate`.
pub fn set_in_resolver_update(
    cs: Arc<dyn ConfigSelector>,
    mut update: ResolverUpdate,
) -> ResolverUpdate {
    update.attributes = update.attributes.add(ConfigSelectorAttr(cs));
    update
}

/// Wrapper that allows an `Arc<dyn ConfigSelector>` to be stored in
/// [`Attributes`](crate::attributes::Attributes), which requires values to
/// implement [`Eq`].
///
/// Equality is based on pointer identity: two wrappers are equal if and only
/// if they point to the same `ConfigSelector` allocation.
#[derive(Debug, Clone)]
struct ConfigSelectorAttr(Arc<dyn ConfigSelector>);

impl PartialEq for ConfigSelectorAttr {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ConfigSelectorAttr {}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::sync::Weak;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::sync::watch;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::Server;
    use tonic_prost::prost::Message as _;

    use super::*;
    use crate::StatusCodeError;
    use crate::async_trait;
    use crate::client::CallOptions;
    use crate::client::Channel;
    use crate::client::DynRecvStream;
    use crate::client::DynSendStream;
    use crate::client::Invoke as _;
    use crate::client::RecvStream as _;
    use crate::client::RequestHeaders;
    use crate::client::ResponseStreamItem;
    use crate::client::SendOptions;
    use crate::client::SendStream as _;
    use crate::client::name_resolution::ChannelController;
    use crate::client::name_resolution::Endpoint;
    use crate::client::name_resolution::Resolver;
    use crate::client::name_resolution::ResolverBuilder;
    use crate::client::name_resolution::ResolverOptions;
    use crate::client::name_resolution::TCP_IP_NETWORK_TYPE;
    use crate::client::name_resolution::Target;
    use crate::client::name_resolution::WorkScheduler;
    use crate::client::name_resolution::global_registry;
    use crate::client::test_util::ByteRecvMsg;
    use crate::client::test_util::ByteSendMsg;
    use crate::core::Address;
    use crate::credentials::ChannelCredentials;
    use crate::credentials::LocalChannelCredentials;
    use crate::credentials::ProtocolInfo;
    use crate::credentials::call::CallCredentials;
    use crate::credentials::client::ClientHandshakeInfo;
    use crate::credentials::client::HandshakeOutput;
    use crate::credentials::common::Authority;
    use crate::echo_pb::EchoRequest;
    use crate::echo_pb::EchoResponse;
    use crate::echo_pb::echo_server::Echo;
    use crate::echo_pb::echo_server::EchoServer;
    use crate::private;
    use crate::rt::BoxEndpoint;
    use crate::rt::GrpcRuntime;

    const UNARY_ECHO_METHOD: &str = "/grpc.examples.echo.Echo/UnaryEcho";
    /// How long to wait for events that are expected to happen.
    const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(10);
    /// How long to wait when checking that an event does not happen.
    const DEFAULT_TEST_SHORT_TIMEOUT: Duration = Duration::from_millis(10);

    /// A config selector that fails every RPC with a fixed status and records
    /// the method name of each RPC it is consulted for.
    #[derive(Debug)]
    struct FakeConfigSelector {
        status: StatusError,
        methods: Mutex<Vec<String>>,
    }

    impl FakeConfigSelector {
        fn new_arc(code: StatusCodeError) -> Arc<Self> {
            Arc::new(Self {
                status: StatusError::new(code, "fake config selector error"),
                methods: Mutex::default(),
            })
        }

        /// Returns the method names of the RPCs this selector was consulted
        /// for, in order.
        fn methods(&self) -> Vec<String> {
            self.methods.lock().unwrap().clone()
        }
    }

    impl ConfigSelector for FakeConfigSelector {
        fn select_config(&self, options: PickOptions) -> Result<RpcConfig, StatusError> {
            self.methods
                .lock()
                .unwrap()
                .push(options.request_headers.method_name().to_string());
            Err(self.status.clone())
        }
    }

    /// A subscription to an xDS cluster. The xDS resolver keeps a cluster
    /// subscribed while its config selector, or an RPC routed to the cluster
    /// that hasn't been committed yet, holds a reference.
    #[derive(Debug)]
    struct ClusterSubscription;

    /// Mimics the xDS resolver's config selector: it holds a reference to a
    /// cluster subscription, and gives each RPC another reference through the
    /// RPC's call attributes.
    #[derive(Debug)]
    struct ClusterConfigSelector {
        subscription: Arc<ClusterSubscription>,
        /// Notified each time a config is selected for an RPC.
        selected_tx: mpsc::UnboundedSender<()>,
    }

    impl ClusterConfigSelector {
        /// Returns a config selector for a new cluster subscription, and a
        /// weak reference for counting the subscription's references. The
        /// selector notifies `selected_tx` each time it selects a config.
        fn new_arc(
            selected_tx: mpsc::UnboundedSender<()>,
        ) -> (Arc<Self>, Weak<ClusterSubscription>) {
            let subscription = Arc::new(ClusterSubscription);
            let refs = Arc::downgrade(&subscription);
            let selector = Self {
                subscription,
                selected_tx,
            };
            (Arc::new(selector), refs)
        }
    }

    impl ConfigSelector for ClusterConfigSelector {
        fn select_config(&self, options: PickOptions) -> Result<RpcConfig, StatusError> {
            options.call_attributes.add(self.subscription.clone());
            // The test may have stopped listening.
            let _ = self.selected_tx.send(());
            Ok(RpcConfig::default())
        }
    }

    /// Returns a resolver scheme that is unique within the test binary.
    /// Resolvers can't be removed from the global registry, so each test
    /// registers its own.
    fn unique_scheme() -> String {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        format!(
            "config-selector-test-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// A resolver update, and a sender for the channel's result of applying
    /// it.
    type PendingUpdate = (ResolverUpdate, oneshot::Sender<Result<(), String>>);

    /// Builds [`FakeResolver`]s, sending a handle to each one to the test.
    struct FakeResolverBuilder {
        scheme: String,
        handle_tx: mpsc::UnboundedSender<FakeResolverHandle>,
    }

    impl FakeResolverBuilder {
        /// Registers a new builder in the global registry. Returns its scheme
        /// and a receiver for the handles of the resolvers it builds.
        fn register() -> (String, mpsc::UnboundedReceiver<FakeResolverHandle>) {
            let scheme = unique_scheme();
            let (handle_tx, handle_rx) = mpsc::unbounded_channel();
            global_registry().add_builder(Box::new(Self {
                scheme: scheme.clone(),
                handle_tx,
            }));
            (scheme, handle_rx)
        }
    }

    impl ResolverBuilder for FakeResolverBuilder {
        fn build(&self, _target: &Target, options: ResolverOptions) -> Box<dyn Resolver> {
            let (update_tx, update_rx) = mpsc::unbounded_channel();
            let _ = self.handle_tx.send(FakeResolverHandle {
                work_scheduler: options.work_scheduler,
                update_tx,
            });
            Box::new(FakeResolver { update_rx })
        }

        fn scheme(&self) -> &str {
            &self.scheme
        }

        fn is_valid_uri(&self, _target: &Target) -> bool {
            true
        }
    }

    /// Lets the test push updates to the channel through a [`FakeResolver`].
    struct FakeResolverHandle {
        work_scheduler: Arc<dyn WorkScheduler>,
        update_tx: mpsc::UnboundedSender<PendingUpdate>,
    }

    impl FakeResolverHandle {
        /// Sends `update` to the channel and returns the channel's result of
        /// applying it.
        async fn update(&self, update: ResolverUpdate) -> Result<(), String> {
            let (result_tx, result_rx) = oneshot::channel();
            self.update_tx.send((update, result_tx)).unwrap();
            self.work_scheduler.schedule_work();
            timeout(DEFAULT_TEST_TIMEOUT, result_rx)
                .await
                .expect("timed out waiting for the channel to apply the update")
                .unwrap()
        }
    }

    /// A resolver that sends the channel the updates pushed through its
    /// [`FakeResolverHandle`].
    struct FakeResolver {
        update_rx: mpsc::UnboundedReceiver<PendingUpdate>,
    }

    impl Resolver for FakeResolver {
        fn resolve_now(&mut self) {}

        fn work(&mut self, channel_controller: &mut dyn ChannelController) {
            while let Ok((update, result_tx)) = self.update_rx.try_recv() {
                let _ = result_tx.send(channel_controller.update(update));
            }
        }
    }

    /// An echo service that only implements unary RPCs.
    struct EchoService;

    #[async_trait]
    impl Echo for EchoService {
        async fn unary_echo(
            &self,
            request: tonic::Request<EchoRequest>,
        ) -> Result<tonic::Response<EchoResponse>, tonic::Status> {
            Ok(tonic::Response::new(EchoResponse {
                message: request.into_inner().message,
            }))
        }

        type ServerStreamingEchoStream = ReceiverStream<Result<EchoResponse, tonic::Status>>;

        async fn server_streaming_echo(
            &self,
            _: tonic::Request<EchoRequest>,
        ) -> Result<tonic::Response<Self::ServerStreamingEchoStream>, tonic::Status> {
            Err(tonic::Status::unimplemented("not used by these tests"))
        }

        async fn client_streaming_echo(
            &self,
            _: tonic::Request<tonic::Streaming<EchoRequest>>,
        ) -> Result<tonic::Response<EchoResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("not used by these tests"))
        }

        type BidirectionalStreamingEchoStream = ReceiverStream<Result<EchoResponse, tonic::Status>>;

        async fn bidirectional_streaming_echo(
            &self,
            _: tonic::Request<tonic::Streaming<EchoRequest>>,
        ) -> Result<tonic::Response<Self::BidirectionalStreamingEchoStream>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("not used by these tests"))
        }
    }

    /// Starts an echo server on a local port and returns its address.
    async fn start_echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = Server::builder()
                .add_service(EchoServer::new(EchoService))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await;
        });
        addr
    }

    /// Local channel credentials that hold every handshake until
    /// [`open`](Self::open) is called. Until then, the channel's subchannels
    /// stay CONNECTING, so its RPCs stay queued waiting for a pick.
    struct GatedChannelCredentials {
        local: LocalChannelCredentials,
        is_open: watch::Sender<bool>,
    }

    impl GatedChannelCredentials {
        fn new_arc() -> Arc<Self> {
            Arc::new(Self {
                local: LocalChannelCredentials::new(),
                is_open: watch::Sender::new(false),
            })
        }

        /// Lets held and future handshakes proceed.
        fn open(&self) {
            self.is_open.send_replace(true);
        }
    }

    #[async_trait]
    impl ChannelCredentials for GatedChannelCredentials {
        fn info(&self) -> &ProtocolInfo {
            self.local.info()
        }

        fn get_call_credentials(
            &self,
            token: private::Internal,
        ) -> Option<&Arc<dyn CallCredentials>> {
            self.local.get_call_credentials(token)
        }

        async fn connect(
            &self,
            authority: &Authority,
            source: BoxEndpoint,
            info: &ClientHandshakeInfo,
            runtime: &GrpcRuntime,
            token: private::Internal,
        ) -> Result<HandshakeOutput, String> {
            // Waiting can't fail, since `self` owns the sender.
            let _ = self.is_open.subscribe().wait_for(|is_open| *is_open).await;
            self.local
                .connect(authority, source, info, runtime, token)
                .await
        }
    }

    /// Starts an echo server and creates a channel that uses a
    /// [`FakeResolver`]. Returns the server's address, the channel, and the
    /// handle of the channel's resolver, which has not sent any updates yet.
    async fn setup() -> (SocketAddr, Channel, FakeResolverHandle) {
        setup_with_credentials(LocalChannelCredentials::new_arc()).await
    }

    /// Like [`setup`], but the channel uses `credentials`.
    async fn setup_with_credentials(
        credentials: Arc<dyn ChannelCredentials>,
    ) -> (SocketAddr, Channel, FakeResolverHandle) {
        let addr = start_echo_server().await;
        let (scheme, mut handle_rx) = FakeResolverBuilder::register();
        let mut channel = Channel::builder(format!("{scheme}:///{addr}"), credentials).build();
        // Exit idle so the channel builds its resolver.
        channel.get_state(true);
        let resolver = timeout(DEFAULT_TEST_TIMEOUT, handle_rx.recv())
            .await
            .expect("timed out waiting for the channel to build its resolver")
            .unwrap();
        (addr, channel, resolver)
    }

    /// Returns a resolver update with a single endpoint for `addr`.
    fn update_for(addr: SocketAddr) -> ResolverUpdate {
        ResolverUpdate {
            endpoints: Ok(vec![Endpoint {
                addresses: vec![Address {
                    network_type: TCP_IP_NETWORK_TYPE,
                    address: addr.to_string().into(),
                    ..Default::default()
                }],
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    /// The streams of an RPC started by [`start_unary_echo`].
    type UnaryEchoRpc = (Box<dyn DynSendStream>, Box<dyn DynRecvStream>);

    /// Starts a unary echo RPC on `channel` without sending its request, so
    /// the RPC stays in progress until it's passed to [`finish_unary_echo`].
    async fn start_unary_echo(channel: &Channel) -> UnaryEchoRpc {
        let headers = RequestHeaders::new().with_method_name(UNARY_ECHO_METHOD);
        timeout(
            DEFAULT_TEST_TIMEOUT,
            channel.invoke(headers, CallOptions::default()),
        )
        .await
        .expect("timed out waiting for the RPC to start")
    }

    /// Starts a unary echo RPC like [`start_unary_echo`], but in a new task.
    /// Returns once `selections` is notified that the RPC's config was
    /// selected. The task outputs the RPC's streams once `invoke` returns.
    async fn spawn_unary_echo(
        channel: &Channel,
        selections: &mut mpsc::UnboundedReceiver<()>,
    ) -> JoinHandle<UnaryEchoRpc> {
        let channel = channel.clone();
        let rpc_handle = tokio::spawn(async move { start_unary_echo(&channel).await });
        timeout(DEFAULT_TEST_TIMEOUT, selections.recv())
            .await
            .expect("timed out waiting for the RPC's config to be selected")
            .unwrap();
        rpc_handle
    }

    /// Sends `message` on an RPC from [`start_unary_echo`] and waits for the
    /// RPC to end. Returns the echoed message, or the status the RPC failed
    /// with.
    async fn finish_unary_echo(rpc: UnaryEchoRpc, message: &str) -> Result<String, StatusError> {
        let (mut tx, mut rx) = rpc;
        let finish = async move {
            let request = Bytes::from(
                EchoRequest {
                    message: message.to_string(),
                }
                .encode_to_vec(),
            );
            // Sending fails if the RPC has already failed. The status is read
            // from the trailers below.
            let _ = tx
                .send(
                    &ByteSendMsg::new(&request),
                    SendOptions::new().with_final_msg(true),
                )
                .await;
            let mut response = ByteRecvMsg::new();
            let mut echoed = None;
            loop {
                match rx.recv(&mut response).await {
                    ResponseStreamItem::Headers(_) => {}
                    ResponseStreamItem::Message => {
                        let data = response.data.take().unwrap();
                        echoed = Some(EchoResponse::decode(data).unwrap().message);
                    }
                    ResponseStreamItem::Trailers(trailers) => {
                        return trailers
                            .into_status()
                            .map(|()| echoed.expect("RPC succeeded without a response"));
                    }
                    ResponseStreamItem::StreamClosed => panic!("stream closed before trailers"),
                }
            }
        };
        timeout(DEFAULT_TEST_TIMEOUT, finish)
            .await
            .expect("timed out waiting for the RPC to end")
    }

    /// Makes a unary echo RPC on `channel`. Returns the echoed message, or the
    /// status the RPC failed with.
    async fn unary_echo(channel: &Channel, message: &str) -> Result<String, StatusError> {
        let rpc = start_unary_echo(channel).await;
        finish_unary_echo(rpc, message).await
    }

    #[tokio::test]
    async fn rpc_succeeds_without_config_selector() {
        let (addr, channel, resolver) = setup().await;
        resolver.update(update_for(addr)).await.unwrap();

        assert_eq!(unary_echo(&channel, "hello").await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn config_selector_error_fails_rpc() {
        let (addr, channel, resolver) = setup().await;
        let selector = FakeConfigSelector::new_arc(StatusCodeError::Unavailable);
        resolver
            .update(set_in_resolver_update(selector.clone(), update_for(addr)))
            .await
            .unwrap();

        let status = unary_echo(&channel, "hello").await.unwrap_err();

        assert_eq!(status.code(), StatusCodeError::Unavailable);
        assert_eq!(status.message(), "fake config selector error");
        assert_eq!(selector.methods(), [UNARY_ECHO_METHOD]);
    }

    #[tokio::test]
    async fn restricted_config_selector_status_becomes_internal() {
        let (addr, channel, resolver) = setup().await;
        // gRFC A54 doesn't allow config selectors to return NOT_FOUND.
        let selector = FakeConfigSelector::new_arc(StatusCodeError::NotFound);
        resolver
            .update(set_in_resolver_update(selector, update_for(addr)))
            .await
            .unwrap();

        let status = unary_echo(&channel, "hello").await.unwrap_err();

        assert_eq!(status.code(), StatusCodeError::Internal);
        assert_eq!(
            status.message(),
            "config selector returned illegal status: NotFound: fake config selector error"
        );
    }

    #[tokio::test]
    async fn rpc_waits_for_first_resolver_update() {
        let (addr, channel, resolver) = setup().await;
        let rpc_channel = channel.clone();
        let mut rpc = tokio::spawn(async move { unary_echo(&rpc_channel, "hello").await });

        assert!(
            timeout(DEFAULT_TEST_SHORT_TIMEOUT, &mut rpc).await.is_err(),
            "RPC completed before the resolver's first update"
        );

        let selector = FakeConfigSelector::new_arc(StatusCodeError::Unavailable);
        resolver
            .update(set_in_resolver_update(selector.clone(), update_for(addr)))
            .await
            .unwrap();

        // The waiting RPC uses the selector from the update, not the default
        // one.
        let status = rpc.await.unwrap().unwrap_err();
        assert_eq!(status.code(), StatusCodeError::Unavailable);
        assert_eq!(status.message(), "fake config selector error");
        assert_eq!(selector.methods(), [UNARY_ECHO_METHOD]);
    }

    #[tokio::test]
    async fn resolver_update_without_config_selector_restores_default() {
        let (addr, channel, resolver) = setup().await;
        let selector = FakeConfigSelector::new_arc(StatusCodeError::Unavailable);
        resolver
            .update(set_in_resolver_update(selector.clone(), update_for(addr)))
            .await
            .unwrap();
        assert!(unary_echo(&channel, "hello").await.is_err());

        resolver.update(update_for(addr)).await.unwrap();

        assert_eq!(unary_echo(&channel, "hello").await.unwrap(), "hello");
        assert_eq!(selector.methods(), [UNARY_ECHO_METHOD]);
    }

    #[tokio::test]
    async fn config_selector_applied_when_lb_policy_rejects_update() {
        let (_, channel, resolver) = setup().await;
        let selector = FakeConfigSelector::new_arc(StatusCodeError::Unavailable);

        // pick_first rejects updates without addresses.
        let result = resolver
            .update(set_in_resolver_update(
                selector.clone(),
                ResolverUpdate::default(),
            ))
            .await;
        assert!(result.is_err());

        // The RPC fails with the config selector's status, not the LB
        // policy's.
        let status = unary_echo(&channel, "hello").await.unwrap_err();
        assert_eq!(status.message(), "fake config selector error");
        assert_eq!(selector.methods(), [UNARY_ECHO_METHOD]);
    }

    #[tokio::test]
    async fn rpcs_hold_cluster_subscription_until_invoke_returns() {
        let credentials = GatedChannelCredentials::new_arc();
        let (addr, channel, resolver) = setup_with_credentials(credentials.clone()).await;
        let (selected_tx, mut selected_rx) = mpsc::unbounded_channel();
        let (selector, cluster_a) = ClusterConfigSelector::new_arc(selected_tx.clone());
        resolver
            .update(set_in_resolver_update(selector, update_for(addr)))
            .await
            .unwrap();
        // The config selector holds one reference.
        assert_eq!(cluster_a.strong_count(), 1);

        // Each RPC holds another one while it's queued waiting for the
        // connection.
        let rpc1_handle = spawn_unary_echo(&channel, &mut selected_rx).await;
        assert_eq!(cluster_a.strong_count(), 2);
        let rpc2_handle = spawn_unary_echo(&channel, &mut selected_rx).await;
        assert_eq!(cluster_a.strong_count(), 3);

        // The RPCs release their references once `invoke` returns, although
        // they're still in progress.
        credentials.open();
        let rpc1 = rpc1_handle.await.unwrap();
        let rpc2 = rpc2_handle.await.unwrap();
        assert_eq!(cluster_a.strong_count(), 1);

        assert_eq!(finish_unary_echo(rpc1, "hello").await.unwrap(), "hello");
        assert_eq!(finish_unary_echo(rpc2, "hello").await.unwrap(), "hello");

        // Replacing the config selector releases the subscription.
        let (selector, cluster_b) = ClusterConfigSelector::new_arc(selected_tx);
        resolver
            .update(set_in_resolver_update(selector, update_for(addr)))
            .await
            .unwrap();
        assert_eq!(cluster_a.strong_count(), 0);
        assert_eq!(cluster_b.strong_count(), 1);
    }

    #[tokio::test]
    async fn queued_rpcs_hold_cluster_subscription_after_config_selector_update() {
        let credentials = GatedChannelCredentials::new_arc();
        let (addr, channel, resolver) = setup_with_credentials(credentials.clone()).await;
        let (selected_tx, mut selected_rx) = mpsc::unbounded_channel();
        let (selector, cluster_a) = ClusterConfigSelector::new_arc(selected_tx.clone());
        resolver
            .update(set_in_resolver_update(selector, update_for(addr)))
            .await
            .unwrap();
        let rpc1_handle = spawn_unary_echo(&channel, &mut selected_rx).await;
        assert_eq!(cluster_a.strong_count(), 2);
        let rpc2_handle = spawn_unary_echo(&channel, &mut selected_rx).await;
        assert_eq!(cluster_a.strong_count(), 3);

        // Replacing the config selector drops its reference, but the queued
        // RPCs keep the subscription.
        let (selector, cluster_b) = ClusterConfigSelector::new_arc(selected_tx);
        resolver
            .update(set_in_resolver_update(selector, update_for(addr)))
            .await
            .unwrap();
        assert_eq!(cluster_a.strong_count(), 2);
        assert_eq!(cluster_b.strong_count(), 1);

        // The subscription is released once the RPCs' `invoke` calls return.
        credentials.open();
        let rpc1 = rpc1_handle.await.unwrap();
        let rpc2 = rpc2_handle.await.unwrap();
        assert_eq!(cluster_a.strong_count(), 0);
        assert_eq!(cluster_b.strong_count(), 1);

        // The RPCs still succeed.
        assert_eq!(finish_unary_echo(rpc1, "hello").await.unwrap(), "hello");
        assert_eq!(finish_unary_echo(rpc2, "hello").await.unwrap(), "hello");
    }
}
