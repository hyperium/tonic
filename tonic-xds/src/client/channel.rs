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

use crate::client::cluster::ClusterClientRegistry;
use crate::client::endpoint::{EndpointAddress, MakeConnector};
use crate::client::lb::{ClusterDiscovery, XdsLbService};
use crate::client::route::{PreRouteInterceptor, Router, XdsRoutingLayer};
use crate::xds::bootstrap::{BootstrapConfig, BootstrapError};
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::{
    CertProviderError, CertProviderRegistry, CertificateProvider, file_watcher::FileWatcherProvider,
};
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider_config::FileWatcherConfig;
use crate::xds::cluster_discovery::{GrpcMakeConnector, XdsClusterDiscovery};
use crate::xds::resource_manager::XdsResourceManager;
use crate::xds::routing::XdsRouter;
use crate::{TonicCallCredentials, XdsUri};
use http::{Request, Response};
use http_body::Body;
use shared_http_body::SharedBody;
#[cfg(feature = "_tls-any")]
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::task::{Context, Poll};
use tonic::{body::Body as TonicBody, client::GrpcService};
use tower::{BoxError, Service, ServiceBuilder, load::Load, util::BoxCloneSyncService};
#[cfg(feature = "_tls-any")]
use xds_client::Error as XdsClientError;
use xds_client::{
    ClientConfig, MetricsRecorder, Node, ProstCodec, TokioRuntime, TonicTransportBuilder, XdsClient,
};

use crate::client::retry::{RetryClassifierFactory, RetryLayer, RetrySharedConfig};

/// Configuration for building [`XdsChannel`] / [`XdsChannelGrpc`].
#[derive(Clone, Debug)]
pub struct XdsChannelConfig {
    target_uri: XdsUri,
    bootstrap: Option<BootstrapConfig>,
    call_creds: Option<Arc<dyn TonicCallCredentials>>,
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
}

impl XdsChannelConfig {
    /// Creates a new config with the given target URI.
    #[must_use]
    pub fn new(target_uri: XdsUri) -> Self {
        Self {
            target_uri,
            bootstrap: None,
            call_creds: None,
            max_decoding_message_size: None,
            max_encoding_message_size: None,
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

    /// Limit the size of a decoded ADS response, in bytes.
    ///
    /// Defaults to tonic's 4 MiB. A control plane whose response exceeds the
    /// limit breaks the stream, so raise it to match the largest resource set
    /// the server sends.
    pub fn with_max_decoding_message_size(mut self, limit: usize) -> Self {
        self.max_decoding_message_size = Some(limit);
        self
    }

    /// Limit the size of an encoded ADS request, in bytes.
    ///
    /// Defaults to tonic's `usize::MAX`.
    pub fn with_max_encoding_message_size(mut self, limit: usize) -> Self {
        self.max_encoding_message_size = Some(limit);
        self
    }
}

/// Errors that can occur when building an [`XdsChannel`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Bootstrap configuration could not be loaded.
    #[error("bootstrap: {0}")]
    Bootstrap(#[from] BootstrapError),
    /// A `certificate_providers` entry in the bootstrap failed to initialize.
    #[cfg(feature = "_tls-any")]
    #[error("certificate provider: {0}")]
    CertProvider(#[from] CertProviderError),
    /// A65 ADS TLS certificate files failed to initialize.
    #[cfg(feature = "_tls-any")]
    #[error("ADS TLS credentials: {0}")]
    AdsTls(#[source] CertProviderError),
}

#[cfg(feature = "_tls-any")]
#[derive(Clone)]
struct AdsTlsConfigProvider {
    file_watcher: Option<Arc<dyn CertificateProvider>>,
}

#[cfg(feature = "_tls-any")]
impl AdsTlsConfigProvider {
    fn new(config: FileWatcherConfig) -> Result<Self, CertProviderError> {
        let file_watcher = if config.has_certificate_files() {
            Some(Arc::new(FileWatcherProvider::new(config)?) as Arc<dyn CertificateProvider>)
        } else {
            None
        };
        Ok(Self { file_watcher })
    }

    fn client_tls_config(&self) -> xds_client::Result<tonic::transport::ClientTlsConfig> {
        let mut tls_config = tonic::transport::ClientTlsConfig::new();
        if let Some(provider) = &self.file_watcher {
            let data = provider
                .fetch()
                .map_err(|error| XdsClientError::TlsConfig(Box::new(error)))?;
            if let Some(roots) = data.roots() {
                tls_config =
                    tls_config.ca_certificate(tonic::transport::Certificate::from_pem(roots));
            } else {
                tls_config = tls_config.with_native_roots();
            }
            if let Some(identity) = data.identity() {
                tls_config = tls_config.identity(tonic::transport::Identity::from_pem(
                    identity.cert_chain(),
                    identity.key(),
                ));
            }
        } else {
            tls_config = tls_config.with_native_roots();
        }
        Ok(tls_config)
    }
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

/// xDS runtime built once from bootstrap (cache, ADS client, resource manager,
/// and — under a TLS feature — the cert-provider registry), shared by
/// [`XdsChannelBuilder::build_grpc_channel`] and
/// [`XdsChannelBuilder::build_transport_channel`].
struct XdsRuntimeParts {
    cache: Arc<XdsCache>,
    xds_client: XdsClient,
    resource_manager: XdsResourceManager,
    #[cfg(feature = "_tls-any")]
    cert_provider_registry: Arc<CertProviderRegistry>,
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

/// A type-erased xDS channel over an arbitrary transport body pair.
///
/// Generalizes [`XdsChannelGrpc`] to any request/response body: the channel is
/// a `Service<Request<B>, Response = Response<ResBody>>`. The routing, retry,
/// and load-balancing stack is identical to the gRPC channel; only the endpoint
/// transport differs. The body handed to the endpoint transport is the
/// retry-buffered [`SharedBody<B>`] so failed attempts can be replayed.
///
/// `Send + Sync + Clone`; cloning is cheap (the inner stack is
/// reference-counted).
pub type XdsTransportChannel<B, ResBody> =
    BoxCloneSyncService<Request<B>, Response<ResBody>, BoxError>;

/// A [`tonic::client::GrpcService`] implementation that can route and load-balance
/// gRPC requests based on xDS configuration.
///
/// `Send + Sync + Clone`. Cloning is cheap (the inner service stack is
/// reference-counted); callers that need exclusive access for
/// [`tower::Service::call`] should clone per call site rather than share a
/// single instance through a lock.
pub type XdsChannelGrpc = XdsTransportChannel<TonicBody, TonicBody>;

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
    retry_classifier_factory: Option<Arc<dyn RetryClassifierFactory>>,
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
            )
            .field(
                "retry_classifier_factory",
                &self
                    .retry_classifier_factory
                    .as_deref()
                    .map_or("None", |f| std::any::type_name_of_val(f)),
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
            retry_classifier_factory: None,
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

    /// Overrides the [`RetryClassifierFactory`] used to compile per-route retry
    /// policies from RDS. By default (`None`) the gRPC
    /// [`GrpcRetryClassifierFactory`](crate::GrpcRetryClassifierFactory) is used.
    ///
    /// A non-gRPC transport supplies its own factory so a route's Envoy
    /// `retry_on` conditions are interpreted for that transport and produce the
    /// matching [`RetryClassifier`](crate::RetryClassifier).
    #[must_use]
    pub fn with_retry_classifier_factory(
        mut self,
        factory: Arc<dyn RetryClassifierFactory>,
    ) -> Self {
        self.retry_classifier_factory = Some(factory);
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
        let parts = self.build_xds_parts()?;
        Ok(self.build_grpc_channel_from_runtime(parts))
    }

    /// Builds the transport-agnostic xDS runtime (cache, ADS client, resource
    /// manager, and — under a TLS feature — the cert-provider registry) from
    /// bootstrap. Shared by the gRPC and transport-generic build entry points.
    fn build_xds_parts(&self) -> Result<XdsRuntimeParts, BuildError> {
        let bootstrap = match self.config.bootstrap.clone() {
            Some(b) => b,
            None => BootstrapConfig::from_env()?,
        };

        let listener_name = self.config.target_uri.target.clone();

        let server_uri = bootstrap.server_uri().to_owned();

        #[allow(unused_mut)]
        let mut transport_builder = TonicTransportBuilder::new();
        #[cfg(feature = "_tls-any")]
        if let Some(tls_config) = bootstrap.tls_config()? {
            let provider = AdsTlsConfigProvider::new(tls_config).map_err(BuildError::AdsTls)?;
            transport_builder =
                transport_builder.with_tls_config_factory(move |_| provider.client_tls_config());
        }
        #[cfg(not(feature = "_tls-any"))]
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

        if let Some(limit) = self.config.max_decoding_message_size {
            transport_builder = transport_builder.with_max_decoding_message_size(limit);
        }
        if let Some(limit) = self.config.max_encoding_message_size {
            transport_builder = transport_builder.with_max_encoding_message_size(limit);
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

        Ok(XdsRuntimeParts {
            cache,
            xds_client,
            resource_manager,
            #[cfg(feature = "_tls-any")]
            cert_provider_registry,
        })
    }

    /// Wires the shared routing / retry / load-balancing stack from a pre-built
    /// [`XdsRuntimeParts`] and a caller-chosen connector, type-erasing it into an
    /// [`XdsTransportChannel`]. Both the gRPC and transport-generic entry points
    /// funnel through here; the connector, the `to_endpoint_req` body
    /// reconstruction, and the `fallback_retry` config differ per transport.
    ///
    /// `fallback_retry` is applied to requests that carry no per-route retry
    /// config; callers pass a transport-appropriate default (see
    /// [`RetrySharedConfig::grpc_default`] vs
    /// [`RetrySharedConfig::connection_default`]).
    fn build_channel<MC, B, ReqBody, ResBody, F>(
        &self,
        parts: XdsRuntimeParts,
        make_connector: MC,
        to_endpoint_req: F,
        fallback_retry: Arc<RetrySharedConfig>,
    ) -> XdsTransportChannel<B, ResBody>
    where
        MC: MakeConnector,
        MC::Service: Service<
                Request<ReqBody>,
                Response = Response<ResBody>,
                Error: Into<BoxError>,
                Future: Send + 'static,
            > + Load<Metric: Debug>,
        F: Fn(Request<SharedBody<B>>) -> Request<ReqBody> + Clone + Send + Sync + 'static,
        B: Body + Unpin + Send + 'static,
        B::Data: Clone + Send + Sync,
        B::Error: Clone + Send + Sync,
        ReqBody: Send + 'static,
        ResBody: Send + 'static,
    {
        let router: Arc<dyn Router> = Arc::new(match self.retry_classifier_factory.clone() {
            Some(factory) => XdsRouter::with_retry_factory(&parts.cache, factory),
            None => XdsRouter::new(&parts.cache),
        });

        // Fallback retry config for requests that carry no per-route config
        // (non-xDS callers, or a route with no retry policy). The caller supplies
        // a transport-appropriate default: the gRPC path stamps the
        // `grpc-previous-rpc-attempts` header on connection retries, while
        // transport-generic callers retry connection errors without it. Per-route
        // configs come from RDS via the request's `RouteDecision`; see `RetryLayer`.
        let retry_layer = RetryLayer::new(fallback_retry);

        #[cfg(feature = "_tls-any")]
        let discovery: Arc<dyn ClusterDiscovery<EndpointAddress, MC::Service>> = Arc::new(
            XdsClusterDiscovery::new(parts.cache, make_connector, parts.cert_provider_registry),
        );
        #[cfg(not(feature = "_tls-any"))]
        let discovery: Arc<dyn ClusterDiscovery<EndpointAddress, MC::Service>> =
            Arc::new(XdsClusterDiscovery::new(parts.cache, make_connector));

        let resources = Arc::new(XdsChannelResources {
            _resource_manager: parts.resource_manager,
            _xds_client: parts.xds_client,
        });

        let routing_layer = XdsRoutingLayer::new(router, self.pre_route.clone(), self.authority());
        self.assemble_channel_stack::<MC::Service, B, ReqBody, ResBody, _>(
            routing_layer,
            retry_layer,
            discovery,
            to_endpoint_req,
            Some(resources),
        )
    }

    /// gRPC specialization of [`build_channel`](Self::build_channel): wires the
    /// tonic `Channel` connector (with the cert-provider registry under a TLS
    /// feature) and reconstructs each endpoint request body as [`TonicBody`].
    /// Separated so tests can inject a disconnected `XdsClient` and a
    /// pre-populated cache via [`XdsRuntimeParts`].
    fn build_grpc_channel_from_runtime(&self, parts: XdsRuntimeParts) -> XdsChannelGrpc {
        self.build_channel::<GrpcMakeConnector, TonicBody, TonicBody, TonicBody, _>(
            parts,
            GrpcMakeConnector::new(),
            |req: Request<SharedBody<TonicBody>>| req.map(TonicBody::new),
            Arc::new(RetrySharedConfig::grpc_default()),
        )
    }

    /// Assembles the routing / retry / load-balancing service stack shared by
    /// every transport, and type-erases it into an [`XdsTransportChannel`].
    ///
    /// The stack is, outermost to innermost: the routing layer (stamps the
    /// matched `RouteDecision`), the retry layer (buffers the body into
    /// [`SharedBody`] and retries per the route's config), a `map_request` that
    /// reconstructs the endpoint's request body from the buffered `SharedBody`,
    /// and the load balancer that dispatches to the discovered endpoints.
    ///
    /// `to_endpoint_req` adapts `Request<SharedBody<B>>` to the endpoint's
    /// `Request<ReqBody>`: the built-in gRPC path rewraps into [`TonicBody`],
    /// while transport-generic callers pass the body through unchanged
    /// (`ReqBody = SharedBody<B>`).
    fn assemble_channel_stack<EpService, B, ReqBody, ResBody, F>(
        &self,
        routing_layer: XdsRoutingLayer,
        retry_layer: RetryLayer,
        discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EpService>>,
        to_endpoint_req: F,
        resources: Option<Arc<XdsChannelResources>>,
    ) -> XdsTransportChannel<B, ResBody>
    where
        EpService: Service<Request<ReqBody>, Response = Response<ResBody>> + Load + Send + 'static,
        EpService::Error: Into<BoxError>,
        EpService::Future: Send + 'static,
        EpService::Metric: Debug,
        F: Fn(Request<SharedBody<B>>) -> Request<ReqBody> + Clone + Send + Sync + 'static,
        B: Body + Unpin + Send + 'static,
        B::Data: Clone + Send + Sync,
        B::Error: Clone + Send + Sync,
        ReqBody: Send + 'static,
        ResBody: Send + 'static,
    {
        let cluster_registry =
            Arc::new(ClusterClientRegistry::<Request<ReqBody>, Response<ResBody>>::new());
        let lb_service = XdsLbService::new(cluster_registry, discovery);
        let inner = ServiceBuilder::new()
            .layer(routing_layer)
            .layer(retry_layer)
            .map_request(to_endpoint_req)
            .service(lb_service);

        BoxCloneSyncService::new(XdsChannel {
            config: self.config.clone(),
            inner,
            _resources: resources,
        })
    }

    /// Builds an `XdsChannelGrpc`, which is a type-erased gRPC channel.
    ///
    /// This is the gRPC specialization of
    /// [`build_transport_channel`](Self::build_transport_channel).
    pub fn build_grpc_channel(&self) -> Result<XdsChannelGrpc, BuildError> {
        self.build_tonic_grpc_channel()
    }

    /// Builds a transport-generic xDS channel using a custom connector factory.
    ///
    /// Generalizes [`build_grpc_channel`](Self::build_grpc_channel) to any
    /// transport: routing, retry, and load balancing are wired identically to
    /// the gRPC channel, but endpoint connections are produced by the supplied
    /// [`MakeConnector`]. The returned channel is a
    /// `Service<Request<B>, Response = Response<ResBody>>`.
    ///
    /// The connector's endpoint service consumes `Request<SharedBody<B>>` — the
    /// retry-buffered request body — so failed attempts can be replayed. As
    /// [`SharedBody<B>`] implements [`http_body::Body`], a hyper/HTTP-based
    /// endpoint can forward it directly.
    ///
    /// # TLS
    ///
    /// The [`ClusterConfig`](crate::ClusterConfig) handed to the connector
    /// exposes the cluster's parsed TLS settings via its `tls()` accessor
    /// (under a TLS feature). A custom connector uses the returned
    /// `ClusterTlsConfig` to build a gRFC-A29-conformant verifier and resolve
    /// the optional mTLS identity — the same path the built-in gRPC connector
    /// takes — without depending on crate-internal registry or matcher types.
    ///
    /// # Retry semantics
    ///
    /// Per-route retry policies are compiled by the [`RetryClassifierFactory`]
    /// set via
    /// [`with_retry_classifier_factory`](Self::with_retry_classifier_factory).
    /// The default gRPC factory only produces status-code retries for gRPC
    /// responses, so a non-gRPC transport supplies its own factory to interpret
    /// `retry_on` (e.g. `5xx`, `gateway-error`) for that transport. Without a
    /// custom factory only connection-error retries take effect.
    pub fn build_transport_channel<MC, B, ResBody>(
        &self,
        make_connector: MC,
    ) -> Result<XdsTransportChannel<B, ResBody>, BuildError>
    where
        MC: MakeConnector,
        MC::Service: Service<
                Request<SharedBody<B>>,
                Response = Response<ResBody>,
                Error: Into<BoxError>,
                Future: Send + 'static,
            > + Load<Metric: Debug>,
        B: Body + Unpin + Send + 'static,
        B::Data: Clone + Send + Sync,
        B::Error: Clone + Send + Sync,
        ResBody: Send + 'static,
    {
        let parts = self.build_xds_parts()?;
        Ok(self.build_channel::<MC, B, SharedBody<B>, ResBody, _>(
            parts,
            make_connector,
            |req: Request<SharedBody<B>>| req,
            Arc::new(RetrySharedConfig::connection_default()),
        ))
    }

    /// Channel-level authority used as the routing key for matching against
    /// `VirtualHost.domains` in RDS.
    fn authority(&self) -> Arc<str> {
        Arc::from(self.config.target_uri.target.as_str())
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
    use crate::client::retry::RetrySharedConfig;
    use crate::client::route::PreRouteInterceptor;
    use crate::client::route::RouteDecision;
    use crate::client::route::RouteInput;
    use crate::client::route::Router;
    use crate::testutil::grpc::GreeterClient;
    use crate::testutil::grpc::HelloRequest;
    use crate::testutil::grpc::TestServer;
    use crate::xds::cache::XdsCache;
    use crate::xds::resource::EndpointsResource;
    use crate::xds::resource::route_config::RouteConfigResource;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use tokio::sync::mpsc;
    use tonic::transport::Channel;
    use tower::discover::Change;

    impl super::XdsChannelBuilder {
        /// Builds an `XdsChannelGrpc` from the given router, cluster discovery, retry
        /// config, and optional pre-route interceptor.
        pub(crate) fn build_grpc_channel_from_parts(
            &self,
            router: Arc<dyn Router>,
            discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>>,
            retry: Arc<RetrySharedConfig>,
            interceptor: Option<Arc<dyn PreRouteInterceptor>>,
        ) -> XdsChannelGrpc {
            use crate::client::retry::RetryLayer;
            use crate::client::route::XdsRoutingLayer;
            use http::Request;
            use shared_http_body::SharedBody;
            use tonic::body::Body as TonicBody;

            let routing_layer = XdsRoutingLayer::new(router, interceptor, self.authority());
            let retry_layer = RetryLayer::new(retry);
            self.assemble_channel_stack::<EndpointChannel<Channel>, TonicBody, TonicBody, TonicBody, _>(
                routing_layer,
                retry_layer,
                discovery,
                |req: Request<SharedBody<TonicBody>>| req.map(TonicBody::new),
                None,
            )
        }
    }

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
        fn acquire(&self) -> crate::client::route::AcquiredConfig {
            crate::client::route::AcquiredConfig::Ready(Arc::new(Default::default()))
        }

        fn route(
            &self,
            _input: &RouteInput<'_>,
            _config: &crate::client::route::RoutingSnapshot,
        ) -> Result<RouteDecision, crate::xds::routing::RoutingError> {
            Ok(RouteDecision {
                cluster: "test-cluster".to_string(),
                request_hash: None,
                retry_config: None,
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
            Arc::new(RetrySharedConfig::grpc_default()),
            None,
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
        use crate::client::retry::{GrpcRetryClassifier, RetryConfig};
        use crate::testutil::grpc::spawn_fail_first_n_server;

        // Server fails the first request with UNAVAILABLE, succeeds on retry.
        let server = spawn_fail_first_n_server("retry-server", 1)
            .await
            .expect("Failed to spawn server");

        let servers = vec![server];
        let xds_manager = Arc::new(MockXdsManager::from_test_servers(&servers));

        let retry = Arc::new(RetrySharedConfig::new(
            RetryConfig::new().num_retries(1),
            Arc::new(GrpcRetryClassifier::new(vec![tonic::Code::Unavailable])),
        ));

        let xds_channel = XdsChannelBuilder::new(test_config()).build_grpc_channel_from_parts(
            xds_manager.clone(),
            xds_manager.clone(),
            retry,
            None,
        );

        let mut client = GreeterClient::new(xds_channel);

        let response = client
            .say_hello(HelloRequest {
                name: "retry-test".to_string(),
            })
            .await
            .expect("request should succeed after retry");

        assert_eq!(response.into_inner().message, "retry-server: retry-test");
    }

    /// Helper: creates a minimal plaintext `ClusterResource` for tests that
    /// drive `XdsClusterDiscovery`. The cluster watch in `discover_cluster`
    /// blocks until a cluster is in the cache.
    fn make_test_cluster(cluster_name: &str) -> Arc<crate::xds::resource::ClusterResource> {
        use crate::xds::resource::cluster::{ClusterResource, LbPolicy};
        Arc::new(ClusterResource {
            name: cluster_name.to_string(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
        })
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
                    retry_config: None,
                }],
            }],
            metadata: Default::default(),
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
    /// backed by the given cache.
    async fn build_xds_channel_from_cache(cache: Arc<XdsCache>) -> XdsChannelGrpc {
        use crate::xds::cluster_discovery::{GrpcMakeConnector, XdsClusterDiscovery};
        use crate::xds::routing::XdsRouter;

        let router: Arc<dyn Router> = Arc::new(XdsRouter::new(&cache));

        #[cfg(feature = "_tls-any")]
        let discovery: Arc<
            dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
        > = {
            use crate::xds::cert_provider::CertProviderRegistry;
            let registry = Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            );
            Arc::new(XdsClusterDiscovery::new(
                cache,
                GrpcMakeConnector::new(),
                registry,
            ))
        };
        #[cfg(not(feature = "_tls-any"))]
        let discovery: Arc<
            dyn ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>>,
        > = Arc::new(XdsClusterDiscovery::new(cache, GrpcMakeConnector::new()));

        let builder = XdsChannelBuilder::new(test_config());
        builder.build_grpc_channel_from_parts(
            router,
            discovery,
            Arc::new(RetrySharedConfig::grpc_default()),
            None,
        )
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
        cache.update_cluster(cluster_name, make_test_cluster(cluster_name));
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
        cache.update_cluster(cluster_name, make_test_cluster(cluster_name));
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

    #[cfg(feature = "_tls-any")]
    #[test]
    fn a65_missing_certificate_file_fails_channel_build() {
        use super::BuildError;
        use crate::BootstrapConfig;
        use crate::xds::cert_provider::CertProviderError;

        let bootstrap = BootstrapConfig::from_json(
            r#"{
                "xds_servers": [{
                    "server_uri": "xds.example.com:443",
                    "channel_creds": [{
                        "type": "tls",
                        "config": {
                            "ca_certificate_file": "/definitely/missing/a65-ca.pem"
                        }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let config = test_config().with_bootstrap(bootstrap);

        let error = XdsChannelBuilder::new(config)
            .build_grpc_channel()
            .unwrap_err();
        assert!(matches!(
            &error,
            BuildError::AdsTls(CertProviderError::FileRead { .. })
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn a65_empty_config_does_not_create_a_file_watcher() {
        use super::AdsTlsConfigProvider;
        use crate::xds::cert_provider_config::FileWatcherConfig;

        let provider = AdsTlsConfigProvider::new(FileWatcherConfig::default()).unwrap();
        assert!(provider.file_watcher.is_none());
        provider.client_tls_config().unwrap();
    }

    /// Smoke test: verifies builder wiring with a disconnected XdsClient
    /// doesn't panic during construction.
    #[tokio::test]
    async fn test_build_grpc_channel_from_runtime_smoke() {
        use super::XdsRuntimeParts;
        use crate::xds::resource_manager::XdsResourceManager;

        let cache = Arc::new(XdsCache::new());
        let xds_client = xds_client::XdsClient::disconnected();
        let resource_manager =
            XdsResourceManager::new(xds_client.clone(), cache.clone(), "test-listener".into());

        let builder = XdsChannelBuilder::new(test_config());

        #[cfg(feature = "_tls-any")]
        let parts = {
            use crate::xds::cert_provider::CertProviderRegistry;
            let cert_provider_registry = Arc::new(
                CertProviderRegistry::from_bootstrap(&Default::default(), Default::default())
                    .unwrap(),
            );
            XdsRuntimeParts {
                cache,
                xds_client,
                resource_manager,
                cert_provider_registry,
            }
        };
        #[cfg(not(feature = "_tls-any"))]
        let parts = XdsRuntimeParts {
            cache,
            xds_client,
            resource_manager,
        };
        let _channel = builder.build_grpc_channel_from_runtime(parts);
        // Construction should succeed without panicking.
    }

    /// Minimal body whose `Clone` `Data`/`Error` satisfy `SharedBody`'s bounds.
    struct TestBody(Option<bytes::Bytes>);

    impl http_body::Body for TestBody {
        type Data = bytes::Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
        }
    }

    /// Endpoint that replies with a fixed `x-echo` marker header.
    #[derive(Clone)]
    struct EchoEndpoint;

    impl tower::Service<super::Request<shared_http_body::SharedBody<TestBody>>> for EchoEndpoint {
        type Response = super::Response<TestBody>;
        type Error = tower::BoxError;
        type Future = crate::common::async_util::BoxFuture<Result<Self::Response, Self::Error>>;
        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(
            &mut self,
            _req: super::Request<shared_http_body::SharedBody<TestBody>>,
        ) -> Self::Future {
            Box::pin(async move {
                let mut resp = super::Response::new(TestBody(None));
                resp.headers_mut()
                    .insert("x-echo", http::HeaderValue::from_static("ok"));
                Ok(resp)
            })
        }
    }

    struct MockHttpDiscovery;

    impl ClusterDiscovery<EndpointAddress, EndpointChannel<EchoEndpoint>> for MockHttpDiscovery {
        fn discover_cluster(
            &self,
            _cluster_name: &str,
        ) -> BoxDiscover<EndpointAddress, EndpointChannel<EchoEndpoint>> {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let addr = EndpointAddress::new("127.0.0.1", 1);
                let change = Change::Insert(addr, EndpointChannel::new(EchoEndpoint));
                let _ = tx.send(Ok(change)).await;
            });
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
        }
    }

    struct StaticRouter;

    impl Router for StaticRouter {
        fn acquire(&self) -> crate::client::route::AcquiredConfig {
            crate::client::route::AcquiredConfig::Ready(Arc::new(Default::default()))
        }
        fn route(
            &self,
            _input: &RouteInput<'_>,
            _config: &crate::client::route::RoutingSnapshot,
        ) -> Result<RouteDecision, crate::xds::routing::RoutingError> {
            Ok(RouteDecision {
                cluster: "test-cluster".to_string(),
                request_hash: None,
                retry_config: None,
            })
        }
    }

    /// The transport-generic stack routes an arbitrary (non-gRPC) body through
    /// routing -> retry -> load balancing to a custom endpoint service.
    #[tokio::test]
    async fn test_transport_generic_channel_routes_non_grpc_body() {
        use crate::client::retry::RetryLayer;
        use crate::client::route::XdsRoutingLayer;
        use tower::{Service, ServiceExt};

        let builder = XdsChannelBuilder::new(test_config());
        let router: Arc<dyn Router> = Arc::new(StaticRouter);
        let discovery: Arc<dyn ClusterDiscovery<EndpointAddress, EndpointChannel<EchoEndpoint>>> =
            Arc::new(MockHttpDiscovery);
        let routing_layer = XdsRoutingLayer::new(router, None, builder.authority());
        let retry_layer = RetryLayer::new(Arc::new(RetrySharedConfig::grpc_default()));

        let mut channel = builder.assemble_channel_stack::<
            EndpointChannel<EchoEndpoint>,
            TestBody,
            shared_http_body::SharedBody<TestBody>,
            TestBody,
            _,
        >(routing_layer, retry_layer, discovery, |req| req, None);

        let request = super::Request::new(TestBody(Some(bytes::Bytes::from_static(b"hi"))));
        let response = channel
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .expect("transport-generic request should route and respond");
        assert_eq!(response.headers().get("x-echo").unwrap(), "ok");
    }

    /// Custom connector whose endpoint consumes the retry-buffered `SharedBody`
    /// directly — the shape a hyper/HTTP transport uses.
    struct MockHttpConnector;

    impl crate::client::endpoint::Connector for MockHttpConnector {
        type Service = EndpointChannel<EchoEndpoint>;
        fn connect(
            &self,
            _addr: &EndpointAddress,
        ) -> crate::common::async_util::BoxFuture<Self::Service> {
            Box::pin(async move { EndpointChannel::new(EchoEndpoint) })
        }
    }

    struct MockHttpMakeConnector;

    impl crate::client::endpoint::MakeConnector for MockHttpMakeConnector {
        type Service = EndpointChannel<EchoEndpoint>;
        fn make_connector(
            &self,
            _cluster: crate::client::endpoint::ClusterConfig<'_>,
        ) -> Result<
            Arc<dyn crate::client::endpoint::Connector<Service = Self::Service> + Send + Sync>,
            tower::BoxError,
        > {
            Ok(Arc::new(MockHttpConnector))
        }
    }

    /// Compile-time proof that the public transport-generic entry point is
    /// satisfiable by a custom `MakeConnector` (the HTTP-transport shape).
    const _: fn() = || {
        let _ =
            XdsChannelBuilder::build_transport_channel::<MockHttpMakeConnector, TestBody, TestBody>;
    };
}
