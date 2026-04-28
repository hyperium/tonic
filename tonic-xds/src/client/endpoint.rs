use crate::common::async_util::BoxFuture;
use std::net::SocketAddr;
use std::sync::{Arc, atomic::AtomicU64, atomic::Ordering};
use std::task::{Context, Poll};
use tower::{Service, load::Load};

/// Represents the host part of an endpoint address
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EndpointAddress {
    /// The IP address or hostname
    host: EndpointHost,
    /// The port number
    port: u16,
}

impl EndpointAddress {
    /// Creates a new `EndpointAddress` from a host string and port.
    ///
    /// Attempts to parse the host as an IP address; falls back to hostname.
    #[allow(dead_code)]
    pub(crate) fn new(host: impl Into<String>, port: u16) -> Self {
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
pub(crate) struct EndpointChannel<S> {
    inner: S,
    in_flight: Arc<AtomicU64>,
}

impl<S> EndpointChannel<S> {
    /// Creates a new `EndpointChannel`.
    /// This should be used by xDS implementations to construct channels to individual endpoints.
    #[allow(dead_code)]
    pub(crate) fn new(inner: S) -> Self {
        Self {
            inner,
            in_flight: Arc::new(AtomicU64::new(0)),
        }
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
pub(crate) trait Connector {
    /// The service type produced by this connector.
    type Service;

    /// Connect to the given endpoint address.
    fn connect(
        &self,
        addr: &EndpointAddress,
    ) -> crate::common::async_util::BoxFuture<Self::Service>;
}

/// Factory for creating per-cluster [`Connector`]s.
///
/// The implementation can use the cluster name to look up cluster-specific
/// config (e.g., TLS settings from xDS CDS, cert providers from A29).
///
/// Both `Service` and `Connector` are exposed as associated types so callers
/// can reference `MC::Service` directly without chaining through
/// `<MC::Connector as Connector>::Service`.
#[allow(dead_code)]
pub(crate) trait MakeConnector: Send + Sync + 'static {
    /// The service type produced by the connector.
    type Service;
    /// The connector type produced for each cluster.
    type Connector: Connector<Service = Self::Service>;

    /// Create a connector for the given cluster.
    fn make_connector(&self, cluster_name: &str) -> std::sync::Arc<Self::Connector>;
}
