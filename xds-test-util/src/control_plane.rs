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
//! `XdsTestControlPlaneService` is a fake xDS ADS control plane for gRPC tests.
//!
//! The xDS config resources are injected through
//! [`set_xds_config`](XdsTestControlPlaneService::set_xds_config).
//!
//! The xDS config resources are served by ADS (Aggregated Discovery Service)
//! protocol and State-of-the-world (SOTW): whenever any resource of a type
//! changes, every subscriber to that type receives all of its subscribed
//! resources of that type.

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use envoy_types::pb::envoy::service::discovery::v3::DiscoveryRequest;
use envoy_types::pb::envoy::service::discovery::v3::DiscoveryResponse;
use envoy_types::pb::envoy::service::discovery::v3::aggregated_discovery_service_server::AggregatedDiscoveryServiceServer;
use envoy_types::pb::google::protobuf::Any;
use strum::IntoEnumIterator;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::config::*;

/// Sender half of a stream's outbound `DiscoveryResponse` channel.
type ResponseSender = mpsc::UnboundedSender<DiscoveryResponse>;

/// Per-(type, stream) subscription bookkeeping.
#[derive(Debug)]
struct Subscription {
    /// Outbound channel used to push responses to this stream.
    sender: ResponseSender,
    /// Resource names currently subscribed for this type on this stream.
    resource_names: HashSet<String>,
    /// Last nonce sent to this stream for this type (starts at 0).
    nonce: u64,
}

/// A versioned snapshot of the resources for a single xDS type.
///
/// Kept in its own module so `version` and `resources` stay private and can only
/// change through [`set`](VersionedResourceBundle::set), which guarantees the
/// version is bumped on every mutation. The version starts at 0 (meaning "never
/// set") and increases monotonically, so each distinct resource state carries a
/// distinct version.
mod bundle {
    use super::Any;
    use super::HashMap;

    /// A versioned snapshot of the resources for a single xDS type.
    #[derive(Debug, Default)]
    pub(super) struct VersionedResourceBundle {
        /// Monotonic version of the current resource set (0 means "never set").
        version: u64,
        /// Resources keyed by name.
        resources: HashMap<String, Any>,
    }

    impl VersionedResourceBundle {
        /// Replaces the resource set and bumps the version.
        pub(super) fn set(&mut self, resources: HashMap<String, Any>) {
            self.resources = resources;
            self.version += 1;
        }

        /// Returns the current version (0 means "never set").
        pub(super) fn version(&self) -> u64 {
            self.version
        }

        /// Returns the current resources, keyed by name.
        pub(super) fn resources(&self) -> &HashMap<String, Any> {
            &self.resources
        }
    }
}

use bundle::VersionedResourceBundle;

/// Mutable control-plane state, guarded by a single mutex.
#[derive(Debug)]
struct State {
    /// Versioned resource bundles, keyed by xDS type URL.
    bundles: HashMap<AdsTypeUrl, VersionedResourceBundle>,
    /// Active subscribers for each xDS resource type, keyed by stream ID.
    subscribers: HashMap<AdsTypeUrl, HashMap<u64, Subscription>>,
}

impl State {
    fn new() -> Self {
        let mut bundles = HashMap::new();
        let mut subscribers = HashMap::new();
        for type_url in AdsTypeUrl::iter() {
            bundles.insert(type_url.clone(), VersionedResourceBundle::default());
            subscribers.insert(type_url.clone(), HashMap::new());
        }
        Self {
            bundles,
            subscribers,
        }
    }
}

/// Shared inner state of the control plane.
#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    next_stream_id: AtomicU64,
}

impl Inner {
    /// Handles a single inbound `DiscoveryRequest` for a stream.
    fn handle_request(&self, stream_id: u64, tx: &ResponseSender, req: DiscoveryRequest) {
        let mut state = self
            .state
            .lock()
            .expect("control plane state should not be poisoned");

        // NACK: a request carrying an error detail rejects the last response.
        if req.error_detail.is_some() {
            tracing::warn!(
                "Received NACK for stream {}: {:?}",
                stream_id,
                req.error_detail
            );
            return;
        }

        let type_url: AdsTypeUrl = match req.type_url.parse() {
            Ok(url) => url,
            Err(_) => {
                tracing::error!(
                    "Received request with empty or invalid type_url for stream {} (type_url: {})",
                    stream_id,
                    req.type_url
                );
                return;
            }
        };

        // Nonce check: if the request's nonce doesn't match the last response's nonce,
        // the management server is allowed to ignore any further DiscoveryRequests for
        // the previous version until a DiscoveryRequest bearing the nonce.
        // Ref: https://www.envoyproxy.io/docs/envoy/latest/api-v3/service/discovery/v3/discovery.proto#service-discovery-v3-discoveryresponse
        if !req.response_nonce.is_empty() {
            let matches = state
                .subscribers
                .get(&type_url)
                .and_then(|streams| streams.get(&stream_id))
                .is_some_and(|sub| sub.nonce.to_string() == req.response_nonce);
            if !matches {
                tracing::warn!(
                    "Received request with mismatched nonce for stream {}: {} (type_url={})",
                    stream_id,
                    req.response_nonce,
                    type_url
                );
                return;
            }
        }
        tracing::debug!(
            "Received ACK {} for stream {} with type_url {}",
            req.response_nonce,
            stream_id,
            type_url
        );

        let requested: HashSet<String> = req.resource_names.into_iter().collect();

        // ACK: identical subscription already recorded, nothing to send.
        if let Some(sub) = state
            .subscribers
            .get(&type_url)
            .and_then(|streams| streams.get(&stream_id))
            && sub.resource_names == requested
        {
            tracing::debug!(
                "Received identical subscription for stream {} with type_url {}",
                stream_id,
                type_url
            );
            return;
        }

        // Borrow the bundle and subscriber tables as disjoint fields so the
        // response can be built from `bundles` while mutating `subscribers`.
        let State {
            bundles,
            subscribers,
            ..
        } = &mut *state;
        // A type that was never configured behaves like an empty bundle at version 0.
        let default_bundle = VersionedResourceBundle::default();
        let bundle = bundles.get(&type_url).unwrap_or(&default_bundle);
        let sub = subscribers
            .entry(type_url.clone())
            .or_default()
            .entry(stream_id)
            .or_insert_with(|| Subscription {
                sender: tx.clone(),
                resource_names: HashSet::new(),
                nonce: 0,
            });
        sub.nonce += 1;
        let response = build_response(
            &type_url,
            bundle.version(),
            sub.nonce,
            &requested,
            bundle.resources(),
        );
        let _ = sub.sender.send(response);
        sub.resource_names = requested;
    }

    /// Removes all subscriptions for a stream when it ends.
    fn remove_stream(&self, stream_id: u64) {
        let mut state = self
            .state
            .lock()
            .expect("control plane state should not be poisoned");
        for streams in state.subscribers.values_mut() {
            streams.remove(&stream_id);
        }
    }
}

/// XdsTestControlPlaneService is a bidi-stream service that acts as a local xDS
/// control plane for tests.
///
/// Clone freely: all clones share the same underlying state, so config injected
/// through one clone is served by another. Inject config with
/// [`set_xds_config`](Self::set_xds_config) and retain a clone to push updates
/// while the server runs.
///
/// The service implements the ADS server trait and can be registered with a
/// tonic server via [`AggregatedDiscoveryServiceServer`].
#[derive(Clone, Debug)]
pub struct XdsTestControlPlaneService {
    inner: Arc<Inner>,
}

impl Default for XdsTestControlPlaneService {
    fn default() -> Self {
        Self::new()
    }
}

impl XdsTestControlPlaneService {
    /// Creates a new control plane with no resources configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::new()),
                next_stream_id: AtomicU64::new(0),
            }),
        }
    }

    /// Sets the full set of resources for `type_url`, replacing any previous
    /// resources of that type, and pushes an update to every current subscriber.
    ///
    /// Each resource is packed into a protobuf `Any` tagged with `type_url`.
    /// Resource types are treated as state-of-the-world.
    pub fn set_xds_config<M: prost::Name>(
        &self,
        type_url: &AdsTypeUrl,
        resources: HashMap<String, M>,
    ) {
        let packed: HashMap<String, Any> = resources
            .into_iter()
            .map(|(name, msg)| {
                (
                    name,
                    Any {
                        // Use the type_url from the message type instead of the AdsTypeUrl enum,
                        // so that the Any is tagged with the correct type for the message itself.
                        // This is necessary because cached resource might be of a different type
                        // than the AdsTypeUrl enum, e.g. the
                        // envoy.service.discovery.v3.Resource wrapper resource type.
                        // Therefore, it's important to use the type_url from the message type to
                        // ensure that clients can correctly decode the Any
                        // into the expected message type.
                        type_url: M::type_url(),
                        value: msg.encode_to_vec(),
                    },
                )
            })
            .collect();
        self.set_packed_xds_config(type_url, packed);
    }

    /// Sets the full set of resources for `type_url`, replacing any previous
    /// resources of that type, and pushes an update to every current
    /// subscriber.
    fn set_packed_xds_config(&self, type_url: &AdsTypeUrl, packed: HashMap<String, Any>) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("control plane state should not be poisoned");

        let State {
            bundles,
            subscribers,
            ..
        } = &mut *state;

        // `set` stores the new resources and bumps the version, so the pushed
        // response carries the new version and never reuses the initial 0.
        let bundle = bundles.entry(type_url.clone()).or_default();
        bundle.set(packed);

        if let Some(streams) = subscribers.get_mut(type_url) {
            for sub in streams.values_mut() {
                sub.nonce += 1;
                let response = build_response(
                    type_url,
                    bundle.version(),
                    sub.nonce,
                    &sub.resource_names,
                    bundle.resources(),
                );
                let _ = sub.sender.send(response);
            }
        }
    }

    /// Returns the currently configured resources for `type_url`, as packed
    /// protobuf `Any`s keyed by resource name.
    ///
    /// For a protobuf-free view suitable for assertions, use
    /// [`resource_names`](Self::resource_names).
    #[must_use]
    pub fn get_current_config(&self, type_url: &AdsTypeUrl) -> HashMap<String, Any> {
        let state = self
            .inner
            .state
            .lock()
            .expect("control plane state should not be poisoned");
        state
            .bundles
            .get(type_url)
            .map(|bundle| bundle.resources().clone())
            .unwrap_or_default()
    }

    /// Returns the names of the currently configured resources for `type_url`,
    /// sorted.
    ///
    /// This is a protocol-agnostic view of what the control plane is serving,
    /// suitable for assertions without depending on protobuf types.
    #[must_use]
    pub fn resource_names(&self, type_url: &AdsTypeUrl) -> Vec<String> {
        let state = self
            .inner
            .state
            .lock()
            .expect("control plane state should not be poisoned");
        state
            .bundles
            .get(type_url)
            .map(|bundle| {
                let mut names: Vec<String> = bundle.resources().keys().cloned().collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    /// Returns the number of active subscribers per ADS resource type.
    #[must_use]
    pub fn get_subscriber_counts(&self) -> HashMap<AdsTypeUrl, usize> {
        let state = self
            .inner
            .state
            .lock()
            .expect("control plane state should not be poisoned");

        state
            .subscribers
            .keys()
            .map(|type_url| {
                (
                    type_url.clone(),
                    state.subscribers.get(type_url).map_or(0, HashMap::len),
                )
            })
            .collect()
    }

    /// Returns true if any active stream is subscribed to `name` under
    /// `type_url`.
    ///
    /// Unlike [`Self::get_subscriber_counts`], this identifies *which* client
    /// is subscribed, so tests with more than one client connected to the same
    /// control plane can assert about them individually.
    #[must_use]
    pub fn has_subscriber_for(&self, type_url: &AdsTypeUrl, name: &str) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .expect("control plane state should not be poisoned");
        state.subscribers.get(type_url).is_some_and(|streams| {
            streams
                .values()
                .any(|sub| sub.resource_names.contains(name))
        })
    }

    /// Serves the ADS control plane on an ephemeral `127.0.0.1` port in a
    /// background task.
    ///
    /// Returns a [`RunningControlPlane`] that exposes the bound address via
    /// [`addr`](RunningControlPlane::addr), derefs to this service so config
    /// setters can be called on it directly, and shuts the server down when
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if binding the ephemeral port fails.
    pub async fn start(&self) -> std::io::Result<RunningControlPlane> {
        self.serve(Server::builder()).await
    }

    /// Serves the ADS control plane over TLS on an ephemeral `127.0.0.1` port.
    ///
    /// Set `client_ca_root` on `tls` to require client certificates, which
    /// makes the handshake itself assert that the client presented the
    /// identity it was configured with (gRFC A65).
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS config is rejected or binding the ephemeral
    /// port fails.
    #[cfg(any(feature = "tls-ring", feature = "tls-aws-lc"))]
    pub async fn start_with_tls(
        &self,
        tls: tonic::transport::ServerTlsConfig,
    ) -> std::io::Result<RunningControlPlane> {
        let builder = Server::builder()
            .tls_config(tls)
            .map_err(std::io::Error::other)?;
        self.serve(builder).await
    }

    async fn serve(&self, mut builder: Server) -> std::io::Result<RunningControlPlane> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let service = self.clone();
        let handle = tokio::spawn(
            builder
                .add_service(AggregatedDiscoveryServiceServer::new(service.clone()))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        Ok(RunningControlPlane {
            service,
            addr,
            handle,
        })
    }
}

/// A running [`XdsTestControlPlaneService`] returned by
/// [`XdsTestControlPlaneService::start`].
///
/// Derefs to the underlying control plane, so config setters (e.g.
/// [`set_xds_config`](XdsTestControlPlaneService::set_xds_config)) can be called
/// on it directly. The server task is aborted when this handle is dropped.
#[derive(Debug)]
pub struct RunningControlPlane {
    service: XdsTestControlPlaneService,
    addr: SocketAddr,
    handle: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl RunningControlPlane {
    /// The address the ADS server is listening on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The underlying control plane service (shared state).
    #[must_use]
    pub fn get_service(&self) -> &XdsTestControlPlaneService {
        &self.service
    }
}

impl Drop for RunningControlPlane {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Tonic serving adapter for the ADS bidi stream.
mod tonic_service {
    use std::pin::Pin;
    use std::sync::atomic::Ordering;

    use envoy_types::pb::envoy::service::discovery::v3::DeltaDiscoveryRequest;
    use envoy_types::pb::envoy::service::discovery::v3::DeltaDiscoveryResponse;
    use envoy_types::pb::envoy::service::discovery::v3::DiscoveryRequest;
    use envoy_types::pb::envoy::service::discovery::v3::DiscoveryResponse;
    use envoy_types::pb::envoy::service::discovery::v3::aggregated_discovery_service_server::AggregatedDiscoveryService;
    use tokio_stream::Stream;
    use tokio_stream::StreamExt;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tonic::Request;
    use tonic::Response;
    use tonic::Status;

    use super::Arc;
    use super::XdsTestControlPlaneService;
    use super::mpsc;

    #[tonic::async_trait]
    impl AggregatedDiscoveryService for XdsTestControlPlaneService {
        type StreamAggregatedResourcesStream =
            Pin<Box<dyn Stream<Item = Result<DiscoveryResponse, Status>> + Send>>;

        async fn stream_aggregated_resources(
            &self,
            request: Request<tonic::Streaming<DiscoveryRequest>>,
        ) -> Result<Response<Self::StreamAggregatedResourcesStream>, Status> {
            let mut inbound = request.into_inner();
            let (tx, rx) = mpsc::unbounded_channel::<DiscoveryResponse>();
            let inner = Arc::clone(&self.inner);
            let stream_id = inner.next_stream_id.fetch_add(1, Ordering::Relaxed);

            tokio::spawn(async move {
                while let Some(item) = inbound.next().await {
                    match item {
                        Ok(req) => inner.handle_request(stream_id, &tx, req),
                        Err(err) => {
                            tracing::error!("Error receiving request: {:?}", err);
                            break;
                        }
                    }
                }
                tracing::info!("Stream {} closed", stream_id);
                inner.remove_stream(stream_id);
            });

            let outbound = UnboundedReceiverStream::new(rx).map(Ok);
            Ok(Response::new(Box::pin(outbound)))
        }

        type DeltaAggregatedResourcesStream =
            Pin<Box<dyn Stream<Item = Result<DeltaDiscoveryResponse, Status>> + Send>>;

        async fn delta_aggregated_resources(
            &self,
            _request: Request<tonic::Streaming<DeltaDiscoveryRequest>>,
        ) -> Result<Response<Self::DeltaAggregatedResourcesStream>, Status> {
            Err(Status::unimplemented(
                "delta ADS is not supported by the test control plane",
            ))
        }
    }
}

/// Builds a `DiscoveryResponse` containing the requested resources that exist.
fn build_response(
    type_url: &AdsTypeUrl,
    version: u64,
    nonce: u64,
    resource_names: &HashSet<String>,
    type_resources: &HashMap<String, Any>,
) -> DiscoveryResponse {
    let resources = resource_names
        .iter()
        .filter_map(|name| type_resources.get(name).cloned())
        .collect();
    DiscoveryResponse {
        version_info: version.to_string(),
        resources,
        type_url: type_url.to_string(),
        nonce: nonce.to_string(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use envoy_types::pb::envoy::config::listener::v3::Listener;

    use super::*;

    #[test]
    fn set_and_get_config() {
        let control_plane = XdsTestControlPlaneService::new();

        let mut listeners = HashMap::new();
        listeners.insert(
            "my-listener".to_string(),
            Listener {
                name: "my-listener".to_string(),
                ..Default::default()
            },
        );
        control_plane.set_xds_config(&AdsTypeUrl::Lds, listeners);

        let config = control_plane.get_current_config(&AdsTypeUrl::Lds);
        assert_eq!(config.len(), 1);
        let packed = config.get("my-listener").expect("listener present");
        assert_eq!(packed.type_url, AdsTypeUrl::Lds.to_string());

        // No streams have connected yet.
        let counts = control_plane.get_subscriber_counts();
        assert_eq!(counts.len(), AdsTypeUrl::iter().count());
        assert_eq!(counts.get(&AdsTypeUrl::Lds), Some(&0));

        // A type that was never configured has no resources.
        assert!(
            control_plane
                .get_current_config(&AdsTypeUrl::Cds)
                .is_empty()
        );
    }

    #[test]
    fn versioned_bundle_bumps_version_on_each_set() {
        let mut bundle = VersionedResourceBundle::default();

        // Starts empty at version 0 ("never set").
        assert_eq!(bundle.version(), 0);
        assert!(bundle.resources().is_empty());

        // Each set replaces the resources and bumps the version.
        bundle.set(HashMap::from([("my-listener".to_string(), Any::default())]));
        assert_eq!(bundle.version(), 1);
        assert_eq!(bundle.resources().len(), 1);

        // Setting again bumps to a distinct version, even back to empty.
        bundle.set(HashMap::new());
        assert_eq!(bundle.version(), 2);
        assert!(bundle.resources().is_empty());
    }
}
