use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, atomic::AtomicU64, atomic::Ordering};
use std::task::{Context, Poll};
use tower::{BoxError, load::Load, Service};
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EndpointHost {
    Ipv4(std::net::Ipv4Addr),
    Ipv6(std::net::Ipv6Addr),
    Hostname(String),
}

/// Represents a validated endpoint address extracted from xDS
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointAddress {
    /// The IP address or hostname
    host: EndpointHost,
    /// The port number
    port: u16,
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

#[derive(Clone, Debug, Default)]
struct InFlightTracker {
    in_flight: Arc<AtomicU64>,
}

impl InFlightTracker {
    fn new(in_flight: Arc<AtomicU64>) -> Self {
        in_flight.fetch_add(1, Ordering::Relaxed);
        Self {
            in_flight
        }
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
    /// Creates a new EndpointChannel.
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
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let in_flight = InFlightTracker::new(self.in_flight.clone());
        let fut = self.inner.call(req);

        // -1 when the inner future completes
        Box::pin(async move {
            let _in_flight_guard = in_flight;
            let res = fut.await;
            res
        })
    }
}

impl<S> Load for EndpointChannel<S> {
    type Metric = u64;
    fn load(&self) -> Self::Metric {
        self.in_flight.load(Ordering::Relaxed)
    }
}
