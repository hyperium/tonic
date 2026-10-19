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

use crate::common::async_util::BoxFuture;
#[cfg(feature = "_tls-any")]
use crate::xds::cert_provider::{CertProviderRegistry, CertificateProvider};
use crate::xds::resource::cluster::ClusterResource;
use crate::xds::resource::security::ClusterSecurityConfig;
use std::net::SocketAddr;
use std::sync::{Arc, atomic::AtomicU64, atomic::Ordering};
use std::task::{Context, Poll};
use tower::{BoxError, Service, load::Load};

/// Represents the host part of an endpoint address
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum EndpointHost {
    Ipv4(std::net::Ipv4Addr),
    Ipv6(std::net::Ipv6Addr),
    Hostname(String),
}

impl From<String> for EndpointHost {
    fn from(s: String) -> Self {
        if let Ok(ipv4) = s.parse::<std::net::Ipv4Addr>() {
            EndpointHost::Ipv4(ipv4)
        } else if let Ok(ipv6) = s.parse::<std::net::Ipv6Addr>() {
            EndpointHost::Ipv6(ipv6)
        } else {
            EndpointHost::Hostname(s)
        }
    }
}

/// Represents a validated endpoint address extracted from xDS
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointAddress {
    /// The IP address or hostname
    host: EndpointHost,
    /// The port number
    port: u16,
}

impl EndpointAddress {
    /// Creates a new `EndpointAddress` from a host string and port.
    ///
    /// Attempts to parse the host as an IP address; falls back to hostname.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: EndpointHost::from(host.into()),
            port,
        }
    }
}

impl std::fmt::Display for EndpointAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            EndpointHost::Ipv4(ip) => write!(f, "{ip}:{}", self.port),
            EndpointHost::Ipv6(ip) => write!(f, "[{ip}]:{}", self.port),
            EndpointHost::Hostname(h) => write!(f, "{h}:{}", self.port),
        }
    }
}

impl From<SocketAddr> for EndpointAddress {
    fn from(addr: SocketAddr) -> Self {
        match addr {
            SocketAddr::V4(v4_addr) => Self {
                host: EndpointHost::Ipv4(*v4_addr.ip()),
                port: v4_addr.port(),
            },
            SocketAddr::V6(v6_addr) => Self {
                host: EndpointHost::Ipv6(*v6_addr.ip()),
                port: v6_addr.port(),
            },
        }
    }
}

/// RAII tracker for in-flight requests.
/// This is mainly used to implement endpoint load reporting for load balancing purposes.
#[derive(Clone, Debug, Default)]
struct InFlightTracker {
    in_flight: Arc<AtomicU64>,
}

impl InFlightTracker {
    fn new(in_flight: Arc<AtomicU64>) -> Self {
        in_flight.fetch_add(1, Ordering::Relaxed);
        Self { in_flight }
    }
}

impl Drop for InFlightTracker {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// An endpoint channel for communicating with a single gRPC endpoint, with load reporting support for load balancing.
pub struct EndpointChannel<S> {
    inner: S,
    in_flight: Arc<AtomicU64>,
}

impl<S> EndpointChannel<S> {
    /// Creates a new `EndpointChannel`.
    /// This should be used by xDS implementations to construct channels to individual endpoints.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            in_flight: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<S> std::fmt::Debug for EndpointChannel<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointChannel")
            .field("in_flight", &self.in_flight.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<S> Clone for EndpointChannel<S>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            in_flight: self.in_flight.clone(),
        }
    }
}

impl<S, Req> Service<Req> for EndpointChannel<S>
where
    S: Service<Req> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let in_flight = InFlightTracker::new(self.in_flight.clone());
        let fut = self.inner.call(req);

        // -1 when the inner future completes
        Box::pin(async move {
            let _in_flight_guard = in_flight;

            fut.await
        })
    }
}

impl<S> Load for EndpointChannel<S> {
    type Metric = u64;
    fn load(&self) -> Self::Metric {
        self.in_flight.load(Ordering::Relaxed)
    }
}

/// Factory for creating connections to endpoints.
///
/// Implementations capture cluster-level config (TLS, HTTP/2 settings, timeouts)
/// at construction time. The implementation handles retries and concurrency
/// internally — the returned future resolves when a connection is established
/// (or is cancelled by dropping).
pub trait Connector {
    /// The service type produced by this connector.
    type Service;

    /// Connect to the given endpoint address.
    fn connect(
        &self,
        addr: &EndpointAddress,
    ) -> crate::common::async_util::BoxFuture<Self::Service>;
}

/// A read-only view of a cluster's parsed xDS configuration, handed to
/// [`MakeConnector::make_connector`] so a factory can build a connector
/// tailored to the cluster.
///
/// Besides the cluster name, the view exposes the cluster's parsed TLS
/// settings via its `tls()` accessor (under a TLS feature). The view is
/// otherwise opaque: internal matcher and registry types are never surfaced.
pub struct ClusterConfig<'a> {
    name: &'a str,
    /// Parsed TLS config for the cluster (`None` = plaintext). Crate-internal;
    /// read publicly through `ClusterConfig::tls`.
    pub(crate) security: Option<&'a ClusterSecurityConfig>,
    /// Cert-provider registry. Ambient here so [`ClusterTlsConfig`] can resolve
    /// provider instance names without the caller handling the registry.
    #[cfg(feature = "_tls-any")]
    registry: &'a CertProviderRegistry,
}

impl<'a> ClusterConfig<'a> {
    /// Builds a view over a validated [`ClusterResource`], carrying the
    /// cert-provider registry used to resolve the cluster's TLS providers.
    #[cfg(feature = "_tls-any")]
    pub(crate) fn from_resource(
        cluster: &'a ClusterResource,
        registry: &'a CertProviderRegistry,
    ) -> Self {
        Self {
            name: &cluster.name,
            security: cluster.security.as_ref(),
            registry,
        }
    }

    /// Builds a view over a validated [`ClusterResource`] (no TLS feature).
    #[cfg(not(feature = "_tls-any"))]
    pub(crate) fn from_resource(cluster: &'a ClusterResource) -> Self {
        Self {
            name: &cluster.name,
            security: cluster.security.as_ref(),
        }
    }

    /// The cluster name.
    pub fn name(&self) -> &str {
        self.name
    }

    /// The cluster's parsed TLS/security configuration, or `None` when the
    /// cluster uses plaintext.
    ///
    /// A custom [`MakeConnector`] uses the returned [`ClusterTlsConfig`] to
    /// build a gRFC-A29-conformant TLS connector —
    /// [`build_verifier`](ClusterTlsConfig::build_verifier) yields the server
    /// certificate verifier and
    /// [`identity_provider`](ClusterTlsConfig::identity_provider) the optional
    /// mTLS identity source — without depending on the crate-internal
    /// cert-provider registry or SAN-matcher types.
    #[cfg(feature = "_tls-any")]
    pub fn tls(&self) -> Option<ClusterTlsConfig<'a>> {
        self.security.map(|security| ClusterTlsConfig {
            security,
            registry: self.registry,
        })
    }
}

/// A read-only view of a cluster's parsed TLS/security configuration.
///
/// Obtained from [`ClusterConfig::tls`]. Lets a custom [`MakeConnector`] build
/// a gRFC-A29-conformant TLS connector without re-implementing SAN matching or
/// certificate-chain validation, and without depending on the crate-internal
/// cert-provider registry or matcher types. The cert-provider registry is
/// resolved internally, so callers never handle it directly.
#[cfg(feature = "_tls-any")]
pub struct ClusterTlsConfig<'a> {
    security: &'a ClusterSecurityConfig,
    registry: &'a CertProviderRegistry,
}

#[cfg(feature = "_tls-any")]
impl std::fmt::Debug for ClusterTlsConfig<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterTlsConfig")
            .field("ca_instance_name", &self.ca_instance_name())
            .field("identity_instance_name", &self.identity_instance_name())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "_tls-any")]
impl ClusterTlsConfig<'_> {
    /// Bootstrap instance name of the CA trust bundle used to validate the
    /// peer's certificate chain.
    pub fn ca_instance_name(&self) -> &str {
        self.security.ca_instance_name()
    }

    /// Bootstrap instance name of the local identity (client certificate).
    /// `Some` implies mTLS is requested for this cluster.
    pub fn identity_instance_name(&self) -> Option<&str> {
        self.security.identity_instance_name()
    }

    /// Build the gRFC-A29 server-certificate verifier for this cluster.
    ///
    /// Returns a rustls [`ServerCertVerifier`] that validates the peer chain
    /// against the CA bundle — re-read from the provider each handshake, so CA
    /// rotation is picked up — and enforces the cluster's SAN matchers.
    ///
    /// Build once per CDS update in [`MakeConnector::make_connector`] and clone
    /// the returned `Arc` per connection; not for the per-request hot path.
    ///
    /// [`ServerCertVerifier`]: crate::ServerCertVerifier
    pub fn build_verifier(
        &self,
    ) -> Result<Arc<dyn rustls::client::danger::ServerCertVerifier>, ClusterTlsError> {
        let ca_provider = self
            .registry
            .get(self.security.ca_instance_name())
            .ok_or_else(|| {
                ClusterTlsError::UnknownCaInstance(self.security.ca_instance_name().to_owned())
            })?
            .clone();
        Ok(self.security.build_verifier(ca_provider))
    }

    /// Resolve the optional mTLS identity provider for this cluster.
    ///
    /// Returns `Ok(None)` when the cluster requests server authentication only
    /// (no client certificate). When `Some`, fetch the identity per connection
    /// (`provider.fetch()`) so identity rotation reaches each new connection.
    pub fn identity_provider(
        &self,
    ) -> Result<Option<Arc<dyn CertificateProvider>>, ClusterTlsError> {
        self.security
            .identity_instance_name()
            .map(|name| {
                self.registry
                    .get(name)
                    .cloned()
                    .ok_or_else(|| ClusterTlsError::UnknownIdentityInstance(name.to_owned()))
            })
            .transpose()
    }
}

/// Errors resolving a cluster's TLS configuration against the cert-provider
/// registry (see [`ClusterTlsConfig`]).
#[cfg(feature = "_tls-any")]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClusterTlsError {
    /// The cluster's CA provider instance is not configured in
    /// `bootstrap.certificate_providers`.
    #[error("CA provider instance '{0}' is not configured in bootstrap.certificate_providers")]
    UnknownCaInstance(String),
    /// The cluster's identity provider instance is not configured in
    /// `bootstrap.certificate_providers`.
    #[error(
        "identity provider instance '{0}' is not configured in bootstrap.certificate_providers"
    )]
    UnknownIdentityInstance(String),
}

impl std::fmt::Debug for ClusterConfig<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("ClusterConfig");
        dbg.field("name", &self.name);
        #[cfg(feature = "_tls-any")]
        dbg.field("tls", &self.tls());
        dbg.finish_non_exhaustive()
    }
}

/// Factory for creating per-cluster [`Connector`]s.
///
/// Given a [`ClusterConfig`] view, the implementation builds a [`Connector`]
/// tailored to that cluster (e.g. selecting TLS vs plaintext and wiring the
/// gRFC A29 cert providers). Returning an `Err` rejects the current cluster
/// update; the caller keeps the previously built connector.
///
/// The connector is returned type-erased as `Arc<dyn Connector>` so an
/// implementation can keep its concrete connector type(s) private and hand
/// back different connectors for different clusters without wrapping them in
/// a single enum.
pub trait MakeConnector: Send + Sync + 'static {
    /// The service type produced by the connectors.
    ///
    /// Must be `Send + 'static` because discovery drives it from a spawned
    /// task and streams endpoint changes carrying it across threads.
    type Service: Send + 'static;

    /// Build a connector for the given cluster.
    fn make_connector(
        &self,
        cluster: ClusterConfig<'_>,
    ) -> Result<Arc<dyn Connector<Service = Self::Service> + Send + Sync>, BoxError>;
}
