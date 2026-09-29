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

//! xDS-backed [`ClusterDiscovery`] implementation.
//!
//! Per cluster, [`XdsClusterDiscovery::discover_cluster`] spawns a task that
//! drives two concurrent watches:
//!
//! 1. The cluster resource watch — produces a fresh [`Connector`] on each
//!    CDS update (e.g. when `transport_socket` changes). The connector is
//!    held inside a [`ConnectorSwap`] so the diff loop reads the latest
//!    snapshot per endpoint connection.
//! 2. The endpoint watch — produces `Change::Insert` / `Change::Remove`
//!    events forwarded to the LB layer.
//!
//! On a CDS update whose security config fails validation, the previous
//! connector is kept and a warning is logged.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};
use tower::BoxError;

use crate::client::endpoint::{
    ClusterConfig, Connector, EndpointAddress, EndpointChannel, MakeConnector,
};
#[cfg(feature = "_tls-any")]
use crate::client::endpoint::{ClusterTlsConfig, ClusterTlsError};
use crate::client::lb::{BoxDiscover, ClusterDiscovery};
use crate::common::async_util::BoxFuture;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::{CertProviderRegistry, CertificateProvider};
use crate::xds::endpoint_manager::{ConnectorSwap, EndpointManager};

/// Buffer capacity for the discovery channel between the spawned task and
/// Tower's LB layer.
const DISCOVER_CHANNEL_CAPACITY: usize = 64;

/// Timeout for establishing a connection to an endpoint.
const ENDPOINT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP/2 keepalive PING interval for endpoint connections.
const ENDPOINT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Time to wait for a keepalive PING ack before closing the connection.
const ENDPOINT_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Apply connection-liveness settings to a per-endpoint `Endpoint`.
///
/// Endpoint channels are created with `connect_lazy`, so a tonic `Channel`
/// reports readiness independently of TCP connectivity. Without these
/// settings a request routed to a black-holed address (e.g. a deleted pod IP,
/// which Kubernetes drops without a RST) hangs on unanswered SYNs, and an
/// established connection to a dead peer is never torn down — either way the
/// endpoint stays "ready" to the LB and requests only die by the caller's
/// deadline. The connect timeout and keepalives turn both cases into prompt
/// transport errors instead.
fn with_liveness_settings(endpoint: Endpoint) -> Endpoint {
    endpoint
        .connect_timeout(ENDPOINT_CONNECT_TIMEOUT)
        .http2_keep_alive_interval(ENDPOINT_KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(ENDPOINT_KEEP_ALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
}

/// xDS-backed cluster discovery, generic over the connector factory `MC`.
///
/// Resolves cluster names into endpoint change streams by watching the
/// [`XdsCache`]. On each CDS update it asks `MC` to build a [`Connector`] for
/// the cluster. The default [`GrpcMakeConnector`] produces gRPC (plaintext or
/// TLS) connectors from the cluster's TLS view ([`ClusterConfig::tls`]),
/// resolving cert-provider instances against the bootstrap-built registry that
/// discovery carries into each [`ClusterConfig`].
pub(crate) struct XdsClusterDiscovery<MC = GrpcMakeConnector> {
    cache: Arc<XdsCache>,
    make_connector: Arc<MC>,
    #[cfg(feature = "_tls-any")]
    registry: Arc<CertProviderRegistry>,
}

impl<MC> XdsClusterDiscovery<MC> {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(
        cache: Arc<XdsCache>,
        make_connector: MC,
        registry: Arc<CertProviderRegistry>,
    ) -> Self {
        Self {
            cache,
            make_connector: Arc::new(make_connector),
            registry,
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(cache: Arc<XdsCache>, make_connector: MC) -> Self {
        Self {
            cache,
            make_connector: Arc::new(make_connector),
        }
    }
}

impl<MC: MakeConnector> ClusterDiscovery<EndpointAddress, MC::Service> for XdsClusterDiscovery<MC> {
    fn discover_cluster(&self, cluster_name: &str) -> BoxDiscover<EndpointAddress, MC::Service> {
        let cache = self.cache.clone();
        let cluster_name = cluster_name.to_string();
        let make_connector = self.make_connector.clone();
        #[cfg(feature = "_tls-any")]
        let registry = self.registry.clone();

        let (tx, rx) = mpsc::channel(DISCOVER_CHANNEL_CAPACITY);

        tokio::spawn(async move {
            let mut cluster_watch = cache.watch_cluster(&cluster_name);

            let connector_swap: ConnectorSwap<MC::Service> = loop {
                let Some(cluster) = cluster_watch.next().await else {
                    return;
                };
                let cluster_config = ClusterConfig::from_resource(
                    &cluster,
                    #[cfg(feature = "_tls-any")]
                    &registry,
                );
                match make_connector.make_connector(cluster_config) {
                    Ok(c) => break Arc::new(ArcSwap::from_pointee(c)),
                    Err(e) => tracing::warn!(
                        cluster = %cluster_name,
                        error = %e,
                        "initial CDS update rejected; awaiting next update",
                    ),
                }
            };

            let manager = EndpointManager::new(Arc::clone(&connector_swap));
            let mut endpoints = manager.discover_endpoints(cache.watch_endpoints(&cluster_name));

            loop {
                tokio::select! {
                    Some(change) = endpoints.next() => {
                        if tx.send(change).await.is_err() {
                            return;
                        }
                    }
                    Some(cluster) = cluster_watch.next() => {
                        let cluster_config = ClusterConfig::from_resource(
                            &cluster,
                            #[cfg(feature = "_tls-any")]
                            &registry,
                        );
                        match make_connector.make_connector(cluster_config) {
                            Ok(new) => connector_swap.store(Arc::new(new)),
                            Err(e) => tracing::warn!(
                                cluster = %cluster_name,
                                error = %e,
                                "CDS update rejected; keeping previous connector",
                            ),
                        }
                    }
                    else => return,
                }
            }
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

/// The default gRPC [`MakeConnector`].
///
/// Builds a plaintext or (under a TLS feature) TLS connector from a cluster's
/// parsed CDS TLS config ([`ClusterConfig::tls`]), resolving cert-provider
/// instances against the registry carried by the cluster view.
pub(crate) struct GrpcMakeConnector;

impl GrpcMakeConnector {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl MakeConnector for GrpcMakeConnector {
    type Service = EndpointChannel<Channel>;

    fn make_connector(
        &self,
        cluster: ClusterConfig<'_>,
    ) -> Result<Arc<dyn Connector<Service = Self::Service> + Send + Sync>, BoxError> {
        build_connector(&cluster).map_err(Into::into)
    }
}

/// Build a [`Connector`] for the given cluster view: plaintext clusters get a
/// [`PlaintextConnector`]; TLS clusters get a [`TlsConnector`] built from the
/// public [`ClusterTlsConfig`] view, or an error when no TLS feature is on.
fn build_connector(
    cluster: &ClusterConfig<'_>,
) -> Result<Arc<dyn Connector<Service = EndpointChannel<Channel>> + Send + Sync>, ConnectorBuildError>
{
    #[cfg(feature = "_tls-any")]
    {
        match cluster.tls() {
            None => Ok(Arc::new(PlaintextConnector)),
            Some(tls) => Ok(Arc::new(TlsConnector::new(&tls)?)),
        }
    }
    #[cfg(not(feature = "_tls-any"))]
    {
        match cluster.security {
            None => Ok(Arc::new(PlaintextConnector)),
            Some(_) => Err(ConnectorBuildError::TlsFeatureMissing),
        }
    }
}

/// Errors building a per-cluster gRPC [`Connector`] from a cluster's parsed
/// security config.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectorBuildError {
    /// TLS connector build failed (unknown provider instance, etc.).
    #[cfg(feature = "_tls-any")]
    #[error("build TLS connector: {0}")]
    Tls(#[from] ClusterTlsError),
    /// The cluster requires TLS but the binary was built without a TLS
    /// crypto backend feature.
    #[cfg(not(feature = "_tls-any"))]
    #[error("cluster requires TLS but no TLS feature enabled (build with tls-ring or tls-aws-lc)")]
    TlsFeatureMissing,
}

/// Plaintext (non-TLS) [`Connector`] that produces a lazily-connected
/// `tonic::Channel` for each endpoint.
pub(crate) struct PlaintextConnector;

impl Connector for PlaintextConnector {
    type Service = EndpointChannel<Channel>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        // EndpointAddress only holds validated Ipv4/Ipv6/Hostname + u16 port,
        // and its Display impl produces "ip:port" or "hostname:port". Prefixing
        // with "http://" always yields a valid URI, so from_shared cannot fail.
        let endpoint = Endpoint::from_shared(format!("http://{addr}"))
            .expect("EndpointAddress Display guarantees valid URI");
        let channel = with_liveness_settings(endpoint).connect_lazy();
        let svc = EndpointChannel::new(channel);
        Box::pin(async move { svc })
    }
}

/// TLS [`Connector`] for clusters whose CDS resource carries an
/// `UpstreamTlsContext`. Holds a verifier that re-reads CA roots from its
/// [`CertificateProvider`] each handshake, and an optional mTLS identity
/// provider fetched per `connect` — so `file_watcher`-driven CA/identity
/// rotation reaches new connections.
///
/// Built from the public [`ClusterTlsConfig`] view, exactly as an out-of-tree
/// connector would. [`build_connector`] rebuilds it on every CDS update, so
/// changed instance names or SAN matchers propagate as the connector swaps.
#[cfg(feature = "_tls-any")]
pub(crate) struct TlsConnector {
    verifier: Arc<dyn rustls::client::danger::ServerCertVerifier>,
    identity_provider: Option<Arc<dyn CertificateProvider>>,
}

#[cfg(feature = "_tls-any")]
impl TlsConnector {
    pub(crate) fn new(tls: &ClusterTlsConfig<'_>) -> Result<Self, ClusterTlsError> {
        Ok(Self {
            verifier: tls.build_verifier()?,
            identity_provider: tls.identity_provider()?,
        })
    }
}

#[cfg(feature = "_tls-any")]
impl Connector for TlsConnector {
    type Service = EndpointChannel<Channel>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        use rustls::client::danger::ServerCertVerifier;

        let verifier: Arc<dyn ServerCertVerifier> = self.verifier.clone();

        // Fetch identity per `connect` so file_watcher-driven rotation reaches
        // each new connection.
        let identity = self
            .identity_provider
            .as_ref()
            .and_then(|p| match p.fetch() {
                Ok(data) => data
                    .identity()
                    .map(|id| tonic::transport::Identity::from_pem(id.cert_chain(), id.key())),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "identity provider fetch failed; falling back to TLS-only",
                    );
                    None
                }
            });

        let mut tls_config = tonic::transport::ClientTlsConfig::new();
        if let Some(id) = identity {
            tls_config = tls_config.identity(id);
        }

        let uri = format!("https://{addr}");
        let endpoint = with_liveness_settings(
            Endpoint::from_shared(uri.clone())
                .expect("EndpointAddress Display guarantees valid URI"),
        );

        let channel = match endpoint.tls_config_with_verifier(tls_config, verifier) {
            Ok(ep) => ep.connect_lazy(),
            Err(e) => {
                // `tls_config_with_verifier` only errors for UDS endpoints,
                // which we never construct; fall back to a non-TLS lazy channel
                // so the misconfig surfaces at the wire, not here.
                tracing::error!(
                    error = %e, address = %addr,
                    "tls_config_with_verifier failed; non-TLS lazy fallback",
                );
                with_liveness_settings(
                    Endpoint::from_shared(uri)
                        .expect("EndpointAddress Display guarantees valid URI"),
                )
                .connect_lazy()
            }
        };
        let svc = EndpointChannel::new(channel);
        Box::pin(async move { svc })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xds::resource::cluster::{ClusterResource, LbPolicy};

    #[cfg(feature = "_tls-any")]
    use crate::xds::cert_provider::verifier::XdsServerCertVerifier;
    #[cfg(feature = "_tls-any")]
    use crate::xds::cert_provider::{CertProviderError, CertificateData, Identity};
    #[cfg(feature = "_tls-any")]
    use crate::xds::resource::security::ClusterSecurityConfig;

    fn plaintext_cluster() -> ClusterResource {
        ClusterResource {
            name: "c".into(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
        }
    }

    #[cfg(feature = "_tls-any")]
    fn tls_cluster(security: ClusterSecurityConfig) -> ClusterResource {
        ClusterResource {
            name: "c".into(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: Some(security),
        }
    }

    #[cfg(feature = "_tls-any")]
    fn security(ca: &str, identity: Option<&str>) -> ClusterSecurityConfig {
        ClusterSecurityConfig::for_test(ca, identity)
    }

    #[cfg(feature = "_tls-any")]
    fn empty_registry() -> CertProviderRegistry {
        use std::collections::HashMap;
        CertProviderRegistry::from_bootstrap(&HashMap::new(), HashMap::new()).unwrap()
    }

    #[cfg(feature = "_tls-any")]
    fn registry_with(providers: &[(&str, Arc<dyn CertificateProvider>)]) -> CertProviderRegistry {
        use std::collections::HashMap;
        let injected: HashMap<String, Arc<dyn CertificateProvider>> = providers
            .iter()
            .map(|(name, p)| ((*name).to_string(), p.clone()))
            .collect();
        CertProviderRegistry::from_bootstrap(&HashMap::new(), injected).unwrap()
    }

    #[cfg(feature = "_tls-any")]
    struct StaticProvider(Arc<CertificateData>);
    #[cfg(feature = "_tls-any")]
    impl CertificateProvider for StaticProvider {
        fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
            Ok(self.0.clone())
        }
    }

    #[cfg(feature = "_tls-any")]
    fn static_roots_provider() -> Arc<dyn CertificateProvider> {
        Arc::new(StaticProvider(Arc::new(CertificateData::RootsOnly {
            roots: Vec::new(),
        })))
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn build_connector_plaintext_tls_feature_on() {
        let cluster = plaintext_cluster();
        let registry = empty_registry();
        let config = ClusterConfig::from_resource(&cluster, &registry);
        assert!(build_connector(&config).is_ok());
    }

    #[cfg(not(feature = "_tls-any"))]
    #[test]
    fn build_connector_plaintext_no_tls() {
        let cluster = plaintext_cluster();
        let config = ClusterConfig::from_resource(&cluster);
        assert!(build_connector(&config).is_ok());
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn grpc_make_connector_plaintext() {
        let make = GrpcMakeConnector::new();
        let cluster = plaintext_cluster();
        let registry = empty_registry();
        assert!(
            make.make_connector(ClusterConfig::from_resource(&cluster, &registry))
                .is_ok()
        );
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn build_connector_tls_unknown_ca() {
        let cluster = tls_cluster(security("missing-ca", None));
        let registry = empty_registry();
        let config = ClusterConfig::from_resource(&cluster, &registry);
        let Err(err) = build_connector(&config) else {
            panic!("expected UnknownCaInstance error");
        };
        assert!(matches!(
            err,
            ConnectorBuildError::Tls(ClusterTlsError::UnknownCaInstance(ref name))
                if name == "missing-ca"
        ));
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn cluster_tls_view_exposes_instance_names() {
        let registry = empty_registry();

        let plaintext = plaintext_cluster();
        assert!(
            ClusterConfig::from_resource(&plaintext, &registry)
                .tls()
                .is_none()
        );

        let mtls = tls_cluster(security("ca", Some("id")));
        let config = ClusterConfig::from_resource(&mtls, &registry);
        let tls = config.tls().expect("TLS cluster yields a view");
        assert_eq!(tls.ca_instance_name(), "ca");
        assert_eq!(tls.identity_instance_name(), Some("id"));

        let server_only = tls_cluster(security("ca", None));
        let config = ClusterConfig::from_resource(&server_only, &registry);
        assert_eq!(config.tls().unwrap().identity_instance_name(), None);
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn cluster_tls_build_verifier() {
        let registry = registry_with(&[("ca", static_roots_provider())]);

        let cluster = tls_cluster(security("ca", None));
        let config = ClusterConfig::from_resource(&cluster, &registry);
        assert!(config.tls().unwrap().build_verifier().is_ok());

        let missing = tls_cluster(security("nope", None));
        let config = ClusterConfig::from_resource(&missing, &registry);
        assert!(matches!(
            config.tls().unwrap().build_verifier(),
            Err(ClusterTlsError::UnknownCaInstance(name)) if name == "nope"
        ));
    }

    #[cfg(feature = "_tls-any")]
    #[test]
    fn cluster_tls_identity_provider() {
        let registry = registry_with(&[
            ("ca", static_roots_provider()),
            ("id", static_roots_provider()),
        ]);

        let server_only = tls_cluster(security("ca", None));
        let config = ClusterConfig::from_resource(&server_only, &registry);
        assert!(config.tls().unwrap().identity_provider().unwrap().is_none());

        let mtls = tls_cluster(security("ca", Some("id")));
        let config = ClusterConfig::from_resource(&mtls, &registry);
        assert!(config.tls().unwrap().identity_provider().unwrap().is_some());

        let bad = tls_cluster(security("ca", Some("nope")));
        let config = ClusterConfig::from_resource(&bad, &registry);
        assert!(matches!(
            config.tls().unwrap().identity_provider(),
            Err(ClusterTlsError::UnknownIdentityInstance(name)) if name == "nope"
        ));
    }

    #[cfg(feature = "_tls-any")]
    #[tokio::test]
    async fn tls_connector_fetches_identity_per_connect() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingIdentity {
            count: AtomicUsize,
            data: Arc<CertificateData>,
        }
        impl CertificateProvider for CountingIdentity {
            fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
                self.count.fetch_add(1, Ordering::Relaxed);
                Ok(self.data.clone())
            }
        }

        let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> =
            Arc::new(XdsServerCertVerifier::new(static_roots_provider(), vec![]));

        let identity_data = Arc::new(CertificateData::IdentityOnly {
            identity: Identity::new(b"cert".to_vec(), b"key".to_vec()),
        });
        let counter = Arc::new(CountingIdentity {
            count: AtomicUsize::new(0),
            data: identity_data,
        });
        let identity_provider: Arc<dyn CertificateProvider> = counter.clone();
        let connector = TlsConnector {
            verifier,
            identity_provider: Some(identity_provider),
        };

        let addr = EndpointAddress::from("1.2.3.4:443".parse::<std::net::SocketAddr>().unwrap());
        let _ = connector.connect(&addr).await;
        let _ = connector.connect(&addr).await;

        assert_eq!(
            counter.count.load(Ordering::Relaxed),
            2,
            "TlsConnector should fetch identity provider on every connect call",
        );
    }
}
