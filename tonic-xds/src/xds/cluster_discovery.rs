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

use arc_swap::ArcSwap;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};

use crate::client::endpoint::{Connector, EndpointAddress, EndpointChannel};
use crate::client::lb::{BoxDiscover, ClusterDiscovery};
use crate::common::async_util::BoxFuture;
use crate::xds::cache::XdsCache;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::verifier::XdsServerCertVerifier;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::{CertProviderRegistry, CertificateProvider};
use crate::xds::endpoint_manager::{ConnectorSwap, EndpointManager};
use crate::xds::resource::ClusterResource;
#[cfg(feature = "_tls-any")]
use crate::xds::resource::security::ClusterSecurityConfig;

/// Buffer capacity for the discovery channel between the spawned task and
/// Tower's LB layer.
const DISCOVER_CHANNEL_CAPACITY: usize = 64;

/// xDS-backed cluster discovery.
///
/// Resolves cluster names into endpoint change streams by watching the
/// [`XdsCache`]. Builds per-cluster [`Connector`]s based on the cluster's
/// [`ClusterSecurityConfig`] (if any) and the bootstrap-built
/// [`CertProviderRegistry`].
pub(crate) struct XdsClusterDiscovery {
    cache: Arc<XdsCache>,
    #[cfg(feature = "_tls-any")]
    cert_provider_registry: Arc<CertProviderRegistry>,
}

impl XdsClusterDiscovery {
    #[cfg(feature = "_tls-any")]
    pub(crate) fn new(cache: Arc<XdsCache>, registry: Arc<CertProviderRegistry>) -> Self {
        Self {
            cache,
            cert_provider_registry: registry,
        }
    }

    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn new(cache: Arc<XdsCache>) -> Self {
        Self { cache }
    }
}

impl ClusterDiscovery<EndpointAddress, EndpointChannel<Channel>> for XdsClusterDiscovery {
    fn discover_cluster(
        &self,
        cluster_name: &str,
    ) -> BoxDiscover<EndpointAddress, EndpointChannel<Channel>> {
        let cache = self.cache.clone();
        let cluster_name = cluster_name.to_string();
        #[cfg(feature = "_tls-any")]
        let registry = self.cert_provider_registry.clone();

        let (tx, rx) = mpsc::channel(DISCOVER_CHANNEL_CAPACITY);

        tokio::spawn(async move {
            let mut cluster_watch = cache.watch_cluster(&cluster_name);

            let connector_swap: ConnectorSwap<EndpointChannel<Channel>> = loop {
                let Some(cluster) = cluster_watch.next().await else {
                    return;
                };
                let result = build_connector(
                    &cluster,
                    #[cfg(feature = "_tls-any")]
                    &registry,
                );
                match result {
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
                        let result = build_connector(
                            &cluster,
                            #[cfg(feature = "_tls-any")]
                            &registry,
                        );
                        match result {
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

/// Build a [`Connector`] for the given cluster.
///
/// - `cluster.security == None` → [`PlaintextConnector`].
/// - `cluster.security == Some(_)` under a TLS feature → [`TlsConnector`].
/// - `cluster.security == Some(_)` without a TLS feature → error.
fn build_connector(
    cluster: &ClusterResource,
    #[cfg(feature = "_tls-any")] registry: &CertProviderRegistry,
) -> Result<Arc<dyn Connector<Service = EndpointChannel<Channel>> + Send + Sync>, ConnectorBuildError>
{
    match &cluster.security {
        None => Ok(Arc::new(PlaintextConnector)),
        #[cfg(feature = "_tls-any")]
        Some(sec) => Ok(Arc::new(TlsConnector::new(registry, sec)?)),
        #[cfg(not(feature = "_tls-any"))]
        Some(_) => Err(ConnectorBuildError::TlsFeatureMissing),
    }
}

/// Errors building a per-cluster [`Connector`] from a [`ClusterResource`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectorBuildError {
    /// TLS connector build failed (unknown provider instance, etc.).
    #[cfg(feature = "_tls-any")]
    #[error("build TLS connector: {0}")]
    Tls(#[from] TlsConnectorBuildError),
    /// The cluster requires TLS but the binary was built without a TLS
    /// crypto backend feature.
    #[cfg(not(feature = "_tls-any"))]
    #[error("cluster requires TLS but no TLS feature enabled (build with tls-ring or tls-aws-lc)")]
    TlsFeatureMissing,
}

/// Errors constructing a [`TlsConnector`].
#[cfg(feature = "_tls-any")]
#[derive(Debug, thiserror::Error)]
pub(crate) enum TlsConnectorBuildError {
    #[error("CA provider instance '{0}' is not configured in bootstrap.certificate_providers")]
    UnknownCaInstance(String),
    #[error(
        "identity provider instance '{0}' is not configured in bootstrap.certificate_providers"
    )]
    UnknownIdentityInstance(String),
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
        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .expect("EndpointAddress Display guarantees valid URI")
            .connect_lazy();
        let svc = EndpointChannel::new(channel);
        Box::pin(async move { svc })
    }
}

/// TLS [`Connector`] for clusters whose CDS resource carries an
/// `UpstreamTlsContext`. Holds:
///
/// - a verifier that reads CA roots from its [`CertificateProvider`] on
///   each handshake (so `file_watcher`-driven CA rotation is picked up
///   automatically), and
/// - an optional identity provider for mTLS — fetched per [`connect`] call
///   so identity rotation is picked up on each new connection.
///
/// The connector is rebuilt by [`build_connector`] on every CDS update, so
/// changes to `ca_instance_name` / `identity_instance_name` / SAN matchers
/// also propagate as the cluster watch swaps the connector.
#[cfg(feature = "_tls-any")]
pub(crate) struct TlsConnector {
    verifier: Arc<XdsServerCertVerifier>,
    identity_provider: Option<Arc<dyn CertificateProvider>>,
}

#[cfg(feature = "_tls-any")]
impl TlsConnector {
    pub(crate) fn new(
        registry: &CertProviderRegistry,
        security: &ClusterSecurityConfig,
    ) -> Result<Self, TlsConnectorBuildError> {
        let ca_provider = registry
            .get(&security.ca_instance_name)
            .ok_or_else(|| {
                TlsConnectorBuildError::UnknownCaInstance(security.ca_instance_name.clone())
            })?
            .clone();
        let verifier = Arc::new(XdsServerCertVerifier::new(
            ca_provider,
            security.san_matchers.clone(),
        ));

        let identity_provider = security
            .identity_instance_name
            .as_ref()
            .map(|name| {
                registry
                    .get(name)
                    .cloned()
                    .ok_or_else(|| TlsConnectorBuildError::UnknownIdentityInstance(name.clone()))
            })
            .transpose()?;

        Ok(Self {
            verifier,
            identity_provider,
        })
    }
}

#[cfg(feature = "_tls-any")]
impl Connector for TlsConnector {
    type Service = EndpointChannel<Channel>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        use rustls::client::danger::ServerCertVerifier;

        let verifier: Arc<dyn ServerCertVerifier> = self.verifier.clone();

        // Identity is fetched per `connect` so file_watcher-driven identity
        // rotation reaches each new connection. `Identity::from_pem` is
        // bytes-only; the rustls parse happens inside `tls_config_with_verifier`.
        let identity = self
            .identity_provider
            .as_ref()
            .and_then(|p| match p.fetch() {
                Ok(data) => data
                    .identity()
                    .map(|id| tonic::transport::Identity::from_pem(&id.cert_chain, &id.key)),
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
        let endpoint = Endpoint::from_shared(uri.clone())
            .expect("EndpointAddress Display guarantees valid URI");

        let channel = match endpoint.tls_config_with_verifier(tls_config, verifier) {
            Ok(ep) => ep.connect_lazy(),
            Err(e) => {
                // tls_config_with_verifier only errors on UDS endpoints
                // (see tonic's endpoint.rs), which we never construct. The
                // defensive fallback returns a non-TLS lazy channel — the
                // request will fail at the wire, surfacing the misconfig.
                tracing::error!(
                    error = %e, address = %addr,
                    "tls_config_with_verifier failed; non-TLS lazy fallback",
                );
                Endpoint::from_shared(uri)
                    .expect("EndpointAddress Display guarantees valid URI")
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

    fn plaintext_cluster() -> ClusterResource {
        ClusterResource {
            name: "c".into(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
        }
    }

    #[cfg(feature = "_tls-any")]
    fn empty_registry() -> CertProviderRegistry {
        use std::collections::HashMap;
        CertProviderRegistry::from_bootstrap(&HashMap::new()).unwrap()
    }

    /// Plaintext dispatch under TLS feature.
    #[cfg(feature = "_tls-any")]
    #[test]
    fn build_connector_plaintext_tls_feature_on() {
        assert!(build_connector(&plaintext_cluster(), &empty_registry()).is_ok());
    }

    /// Plaintext dispatch without any TLS feature.
    #[cfg(not(feature = "_tls-any"))]
    #[test]
    fn build_connector_plaintext_no_tls() {
        assert!(build_connector(&plaintext_cluster()).is_ok());
    }

    /// Cluster with TLS pointing at an instance not in the registry surfaces
    /// a clear error — useful for misconfig diagnostics.
    #[cfg(feature = "_tls-any")]
    #[test]
    fn build_connector_tls_unknown_ca() {
        use crate::xds::resource::security::ClusterSecurityConfig;

        let mut cluster = plaintext_cluster();
        cluster.security = Some(ClusterSecurityConfig {
            ca_instance_name: "missing-ca".into(),
            identity_instance_name: None,
            san_matchers: vec![],
        });
        let Err(err) = build_connector(&cluster, &empty_registry()) else {
            panic!("expected UnknownCaInstance error");
        };
        assert!(matches!(
            err,
            ConnectorBuildError::Tls(TlsConnectorBuildError::UnknownCaInstance(ref name))
                if name == "missing-ca"
        ));
    }

    /// `TlsConnector::connect` fetches the identity provider on every call,
    /// which is what gives us identity rotation between CDS updates. Counter
    /// shim verifies the call count without standing up a TLS handshake.
    #[cfg(feature = "_tls-any")]
    #[tokio::test]
    async fn tls_connector_fetches_identity_per_connect() {
        use crate::xds::cert_provider::{
            CertProviderError, CertificateData, CertificateProvider, Identity,
        };
        use rustls::RootCertStore;
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

        struct StaticCa(Arc<CertificateData>);
        impl CertificateProvider for StaticCa {
            fn fetch(&self) -> Result<Arc<CertificateData>, CertProviderError> {
                Ok(self.0.clone())
            }
        }

        let ca_provider: Arc<dyn CertificateProvider> =
            Arc::new(StaticCa(Arc::new(CertificateData::RootsOnly {
                roots: Arc::new(RootCertStore::empty()),
            })));
        let verifier = Arc::new(XdsServerCertVerifier::new(ca_provider, vec![]));

        let identity_data = Arc::new(CertificateData::IdentityOnly {
            identity: Identity {
                cert_chain: b"cert".to_vec(),
                key: b"key".to_vec(),
            },
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
