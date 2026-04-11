//! Type-state wrappers for LbChannel lifecycle management.
//!
//! Each state is a separate struct, and transitions consume the old state (move semantics).
//! This prevents using a channel in an invalid state at compile time.
//!
//! ```text
//! Idle → Connecting → Ready ⇄ Ejected
//!                       ↓        ↓
//!                   Connecting  Connecting (via reconnect)
//! ```

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tower::load::Load;
use tower::{BoxError, Service};

use crate::client::endpoint::{Connector, EndpointAddress};
use crate::client::loadbalance::channel::LbChannel;
use crate::common::async_util::BoxFuture;

/// Configuration for an ejected channel.
#[derive(Debug, Clone)]
pub(crate) struct EjectionConfig {
    /// How long the channel is ejected before it can return to service.
    pub timeout: Duration,
    /// Whether the channel needs a fresh connection after ejection expires.
    pub needs_reconnect: bool,
}

/// Result of an ejection expiring.
pub(crate) enum UnejectedChannel<S> {
    /// The channel is ready to serve again (ejection expired, no reconnect needed).
    Ready(ReadyLbChannel<S>),
    /// A fresh connection has been started.
    Connecting(ConnectingLbChannel<S>),
}

// ---------------------------------------------------------------------------
// IdleLbChannel
// ---------------------------------------------------------------------------

/// An idle channel that only stores an address. It is the entry point for
/// starting a connection attempt.
pub(crate) struct IdleLbChannel {
    addr: EndpointAddress,
}

impl IdleLbChannel {
    pub(crate) fn new(addr: EndpointAddress) -> Self {
        Self { addr }
    }

    /// Start connecting to the endpoint. Consumes the idle channel.
    pub(crate) fn connect<C: Connector>(
        self,
        connector: Arc<C>,
    ) -> ConnectingLbChannel<C::Service> {
        let fut = connector.connect(&self.addr);
        ConnectingLbChannel {
            addr: self.addr,
            fut: Some(fut),
        }
    }
}

// ---------------------------------------------------------------------------
// ConnectingLbChannel
// ---------------------------------------------------------------------------

/// A channel that is in the process of connecting.
///
/// Implements [`Stream`] for integration with `StreamMap`. The stream yields
/// exactly one item — `ReadyLbChannel` on success — then terminates.
/// If dropped (e.g., removed from StreamMap), the connection attempt is cancelled.
pub(crate) struct ConnectingLbChannel<S> {
    addr: EndpointAddress,
    fut: Option<BoxFuture<S>>,
}

impl<S: Send + 'static> Stream for ConnectingLbChannel<S> {
    type Item = ReadyLbChannel<S>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(fut) = this.fut.as_mut() else {
            return Poll::Ready(None);
        };
        match fut.as_mut().poll(cx) {
            Poll::Ready(svc) => {
                this.fut = None;
                Poll::Ready(Some(ReadyLbChannel {
                    channel: LbChannel::new(this.addr.clone(), svc),
                }))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// ReadyLbChannel
// ---------------------------------------------------------------------------

/// A channel that is connected and ready to serve requests.
///
/// Wraps an [`LbChannel`] and delegates [`Service`] and [`Load`] to it.
/// State transitions consume `self` to prevent use-after-transition.
pub(crate) struct ReadyLbChannel<S> {
    channel: LbChannel<S>,
}

impl<S> ReadyLbChannel<S> {
    /// Eject this channel (e.g., due to outlier detection). Consumes self.
    pub(crate) fn eject<C>(self, config: EjectionConfig, connector: Arc<C>) -> EjectedLbChannel<S>
    where
        C: Connector<Service = S> + Send + Sync + 'static,
    {
        let ejection_timer = Box::pin(tokio::time::sleep(config.timeout));
        EjectedLbChannel {
            channel: Some(self.channel),
            config,
            connector,
            ejection_timer,
        }
    }

    /// Start reconnecting this channel. Consumes self, dropping the old connection.
    pub(crate) fn reconnect<C: Connector<Service = S>>(
        self,
        connector: Arc<C>,
    ) -> ConnectingLbChannel<S> {
        let addr = self.channel.addr().clone();
        let fut = connector.connect(&addr);
        ConnectingLbChannel {
            addr,
            fut: Some(fut),
        }
    }
}

impl<S, Req> Service<Req> for ReadyLbChannel<S>
where
    S: Service<Req> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError>,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<Result<S::Response, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.channel.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self.channel.call(req)
    }
}

impl<S> Load for ReadyLbChannel<S> {
    type Metric = u64;

    fn load(&self) -> Self::Metric {
        self.channel.load()
    }
}

// ---------------------------------------------------------------------------
// EjectedLbChannel
// ---------------------------------------------------------------------------

/// A channel that has been ejected and is cooling down.
///
/// The underlying connection is kept alive but cannot serve requests.
/// Implements [`Stream`] for integration with `StreamMap`. After the ejection
/// timer expires, yields either:
/// - [`UnejectedChannel::Ready`] if no reconnect is needed
/// - [`UnejectedChannel::Connecting`] if a fresh connection is required
///
/// This is a one-shot stream — it yields exactly one item then terminates.
pub(crate) struct EjectedLbChannel<S> {
    /// Option to allow moving the channel out via `take()` in `poll_next`.
    channel: Option<LbChannel<S>>,
    config: EjectionConfig,
    /// Trait object for the connector. The `Send + Sync` bounds ensure
    /// `EjectedLbChannel<S>` is `Send + Sync` when `S` is (e.g. `tonic::Channel`),
    /// enabling use with `StreamMap` across async task boundaries.
    connector: Arc<dyn Connector<Service = S> + Send + Sync>,
    ejection_timer: Pin<Box<tokio::time::Sleep>>,
}

impl<S: Clone + Unpin> Stream for EjectedLbChannel<S> {
    type Item = UnejectedChannel<S>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(channel) = this.channel.as_ref() else {
            return Poll::Ready(None);
        };
        match this.ejection_timer.as_mut().poll(cx) {
            Poll::Ready(()) => {
                if this.config.needs_reconnect {
                    let addr = channel.addr().clone();
                    this.channel = None;
                    let fut = this.connector.connect(&addr);
                    Poll::Ready(Some(UnejectedChannel::Connecting(ConnectingLbChannel {
                        addr,
                        fut: Some(fut),
                    })))
                } else {
                    match this.channel.take() {
                        Some(channel) => {
                            Poll::Ready(Some(UnejectedChannel::Ready(ReadyLbChannel { channel })))
                        }
                        None => Poll::Ready(None),
                    }
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio_stream::StreamExt;

    #[derive(Clone)]
    struct MockService;

    impl Service<&'static str> for MockService {
        type Response = &'static str;
        type Error = BoxError;
        type Future = future::Ready<Result<&'static str, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: &'static str) -> Self::Future {
            future::ready(Ok("ok"))
        }
    }

    struct MockConnector {
        connect_count: Arc<AtomicU32>,
    }

    impl MockConnector {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                connect_count: Arc::new(AtomicU32::new(0)),
            })
        }
    }

    impl Connector for MockConnector {
        type Service = MockService;

        fn connect(&self, _addr: &EndpointAddress) -> BoxFuture<Self::Service> {
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(future::ready(MockService))
        }
    }

    fn test_addr() -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", 8080)
    }

    #[tokio::test]
    async fn test_idle_to_connecting() {
        let connector = MockConnector::new();
        let idle = IdleLbChannel::new(test_addr());
        let _connecting = idle.connect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_connecting_stream_yields_ready() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector);
        assert!(connecting.next().await.is_some());
    }

    #[tokio::test]
    async fn test_connecting_stream_is_one_shot() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector);
        let _ = connecting.next().await;
        assert!(connecting.next().await.is_none());
    }

    #[tokio::test]
    async fn test_ready_service_delegates() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector);
        let mut ready = connecting.next().await.unwrap();

        let resp = ready.call("hello").await.unwrap();
        assert_eq!(resp, "ok");
    }

    #[tokio::test]
    async fn test_ready_to_connecting_via_reconnect() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector.clone());
        let ready = connecting.next().await.unwrap();

        let mut reconnecting = ready.reconnect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
        assert!(reconnecting.next().await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_yields_ready_after_timeout() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector.clone());
        let ready = connecting.next().await.unwrap();

        let config = EjectionConfig {
            timeout: Duration::from_secs(5),
            needs_reconnect: false,
        };
        let mut ejected = ready.eject(config, connector);

        let result = ejected.next().await.unwrap();
        assert!(matches!(result, UnejectedChannel::Ready(_)));
        assert!(ejected.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_yields_connecting_when_needs_reconnect() {
        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector.clone());
        let ready = connecting.next().await.unwrap();

        let config = EjectionConfig {
            timeout: Duration::from_secs(5),
            needs_reconnect: true,
        };
        let mut ejected = ready.eject(config, connector.clone());

        let result = ejected.next().await.unwrap();
        match result {
            UnejectedChannel::Connecting(mut c) => {
                assert!(c.next().await.is_some());
                assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
            }
            _ => panic!("expected UnejectedChannel::Connecting"),
        }
        assert!(ejected.next().await.is_none());
    }

    #[tokio::test]
    async fn test_connecting_in_stream_map() {
        use tokio_stream::StreamMap;

        let connector = MockConnector::new();
        let connecting = IdleLbChannel::new(test_addr()).connect(connector.clone());

        let mut map: StreamMap<&str, ConnectingLbChannel<MockService>> = StreamMap::new();
        map.insert("endpoint-1", connecting);

        let (key, _ready) = map.next().await.unwrap();
        assert_eq!(key, "endpoint-1");
        // Stream is done, removed from map — map is now empty.
        assert!(map.next().await.is_none());

        // Map can be reused: insert a new stream and it yields correctly.
        let connecting2 = IdleLbChannel::new(test_addr()).connect(connector);
        map.insert("endpoint-2", connecting2);
        let (key2, _ready2) = map.next().await.unwrap();
        assert_eq!(key2, "endpoint-2");
        assert!(map.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_stream_map() {
        use tokio_stream::StreamMap;

        let connector = MockConnector::new();
        let mut connecting = IdleLbChannel::new(test_addr()).connect(connector.clone());
        let ready = connecting.next().await.unwrap();

        let config = EjectionConfig {
            timeout: Duration::from_secs(5),
            needs_reconnect: false,
        };
        let ejected = ready.eject(config, connector.clone());

        let mut map: StreamMap<&str, EjectedLbChannel<MockService>> = StreamMap::new();
        map.insert("endpoint-1", ejected);

        let (key, unejected) = map.next().await.unwrap();
        assert_eq!(key, "endpoint-1");
        assert!(matches!(unejected, UnejectedChannel::Ready(_)));
        // Stream is done, removed from map — map is now empty.
        assert!(map.next().await.is_none());

        // Map can be reused: insert a new ejected stream and it yields correctly.
        let mut connecting2 = IdleLbChannel::new(test_addr()).connect(connector.clone());
        let ready2 = connecting2.next().await.unwrap();
        let ejected2 = ready2.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: false,
            },
            connector,
        );
        map.insert("endpoint-2", ejected2);
        let (key2, unejected2) = map.next().await.unwrap();
        assert_eq!(key2, "endpoint-2");
        assert!(matches!(unejected2, UnejectedChannel::Ready(_)));
        assert!(map.next().await.is_none());
    }
}
