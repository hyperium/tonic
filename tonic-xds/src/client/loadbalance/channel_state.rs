//! Type-state wrappers for LbChannel lifecycle management.
//!
//! Each state is a separate struct, and transitions consume the old state (move semantics).
//! This prevents using a channel in an invalid state at compile time.
//!
//! ```text
//!                +-----------+
//!                |           |
//!                v           |
//! Idle --> Connecting --> Ready <--+--> Ejected
//!                ^                       |
//!                |                       |
//!                +-----------------------+
//! ```
//!
//! State changes are all one-shot. [`ConnectingChannel`] and [`EjectedChannel`] are
//! [`Future`]. The caller (typically a pool) uses [`KeyedFutures`] to
//! manage multiple in-flight state changes and handle cancellation by key.
//!
//! The state types hold the raw service `S` directly. In-flight tracking and
//! load reporting are handled separately by [`LbChannel`] at the pool level.
//!
//! [`KeyedFutures`]: crate::client::loadbalance::keyed_futures::KeyedFutures
//! [`LbChannel`]: crate::client::loadbalance::channel::LbChannel

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use pin_project_lite::pin_project;
use tower::Service;
use tower::load::Load;

use crate::client::endpoint::{Connector, EndpointAddress};
use crate::common::async_util::BoxFuture;

/// Configuration for an ejected channel.
#[derive(Debug, Clone)]
pub(crate) struct EjectionConfig {
    /// How long the channel is ejected before it can return to service.
    pub timeout: Duration,
    /// Whether the channel needs a fresh connection after ejection expires (e.g. after consecutive timeouts).
    pub needs_reconnect: bool,
}

/// Result of an ejection expiring.
pub(crate) enum UnejectedChannel<S> {
    /// The channel is ready to serve again (ejection expired, no reconnect needed).
    Ready(ReadyChannel<S>),
    /// A fresh connection has been started.
    Connecting(ConnectingChannel<S>),
}

// ---------------------------------------------------------------------------
// IdleChannel
// ---------------------------------------------------------------------------

/// An idle channel that only stores an address. It is the entry point for
/// starting a connection attempt.
pub(crate) struct IdleChannel {
    addr: EndpointAddress,
}

impl IdleChannel {
    pub(crate) fn new(addr: EndpointAddress) -> Self {
        Self { addr }
    }

    /// Start connecting to the endpoint. Consumes the idle channel.
    pub(crate) fn connect<C: Connector>(self, connector: Arc<C>) -> ConnectingChannel<C::Service>
    where
        C::Service: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr)
    }
}

// ---------------------------------------------------------------------------
// ConnectingChannel
// ---------------------------------------------------------------------------

/// A channel that is in the process of connecting.
///
/// Implements [`Future`] -- resolves to [`ReadyChannel`] when connected.
/// Cancellation is handled externally via [`KeyedFutures::cancel`].
///
/// [`KeyedFutures::cancel`]: crate::client::loadbalance::keyed_futures::KeyedFutures::cancel
pub(crate) struct ConnectingChannel<S> {
    inner: Pin<Box<dyn Future<Output = ReadyChannel<S>> + Send>>,
}

impl<S: Send + 'static> ConnectingChannel<S> {
    pub(crate) fn new(fut: BoxFuture<S>, addr: EndpointAddress) -> Self {
        Self {
            inner: Box::pin(async move {
                ReadyChannel {
                    addr,
                    inner: fut.await,
                }
            }),
        }
    }
}

impl<S: Send + 'static> Future for ConnectingChannel<S> {
    type Output = ReadyChannel<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().poll(cx)
    }
}

// ---------------------------------------------------------------------------
// ReadyChannel
// ---------------------------------------------------------------------------

/// A channel that is connected and ready to serve requests.
///
/// Holds the raw service `S` and delegates [`Service`] calls directly,
/// preserving `S::Future` and `S::Error` with no wrapping or type erasure.
#[derive(Clone)]
pub(crate) struct ReadyChannel<S> {
    addr: EndpointAddress,
    inner: S,
}

impl<S> ReadyChannel<S> {
    /// Eject this channel (e.g., due to outlier detection). Consumes self.
    pub(crate) fn eject<C>(self, config: EjectionConfig, connector: Arc<C>) -> EjectedChannel<S>
    where
        C: Connector<Service = S> + Send + Sync + 'static,
    {
        let ejection_timer = tokio::time::sleep(config.timeout);
        EjectedChannel {
            addr: self.addr,
            inner: self.inner,
            config,
            connector,
            ejection_timer,
        }
    }

    /// Start reconnecting. Consumes self, dropping the old connection.
    pub(crate) fn reconnect<C: Connector<Service = S>>(
        self,
        connector: Arc<C>,
    ) -> ConnectingChannel<S>
    where
        S: Send + 'static,
    {
        ConnectingChannel::new(connector.connect(&self.addr), self.addr)
    }
}

impl<S, Req> Service<Req> for ReadyChannel<S>
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        self.inner.call(req)
    }
}

impl<S: Load> Load for ReadyChannel<S> {
    type Metric = S::Metric;

    fn load(&self) -> Self::Metric {
        self.inner.load()
    }
}

// ---------------------------------------------------------------------------
// EjectedChannel
// ---------------------------------------------------------------------------

pin_project! {
    /// A channel that has been ejected and is cooling down.
    ///
    /// The underlying connection is kept alive but cannot serve requests.
    /// Implements [`Future`] -- resolves once the ejection timer expires to either:
    /// - [`UnejectedChannel::Ready`] if no reconnect is needed
    /// - [`UnejectedChannel::Connecting`] if a fresh connection is required
    pub(crate) struct EjectedChannel<S> {
        addr: EndpointAddress,
        inner: S,
        config: EjectionConfig,
        connector: Arc<dyn Connector<Service = S> + Send + Sync>,
        #[pin]
        ejection_timer: tokio::time::Sleep,
    }
}

impl<S: Clone + Send + 'static> Future for EjectedChannel<S> {
    type Output = UnejectedChannel<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.ejection_timer.poll(cx) {
            Poll::Ready(()) => {
                if this.config.needs_reconnect {
                    let fut = this.connector.connect(this.addr);
                    Poll::Ready(UnejectedChannel::Connecting(ConnectingChannel::new(
                        fut,
                        this.addr.clone(),
                    )))
                } else {
                    Poll::Ready(UnejectedChannel::Ready(ReadyChannel {
                        addr: this.addr.clone(),
                        inner: this.inner.clone(),
                    }))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::loadbalance::keyed_futures::KeyedFutures;
    use futures_util::task::noop_waker;
    use std::future;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone, Debug)]
    struct MockService;

    impl Service<&'static str> for MockService {
        type Response = &'static str;
        type Error = &'static str;
        type Future = future::Ready<Result<&'static str, &'static str>>;

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

    fn noop_cx() -> Context<'static> {
        Context::from_waker(Box::leak(Box::new(noop_waker())))
    }

    #[tokio::test]
    async fn test_idle_to_connecting() {
        let connector = MockConnector::new();
        let _connecting = IdleChannel::new(test_addr()).connect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_connecting_future_yields_ready() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr()).connect(connector).await;
        assert_eq!(ready.addr, test_addr());
    }

    #[tokio::test]
    async fn test_ready_service_delegates() {
        let connector = MockConnector::new();
        let mut ready = IdleChannel::new(test_addr()).connect(connector).await;
        let resp: &str = ready.call("hello").await.unwrap();
        assert_eq!(resp, "ok");
    }

    #[tokio::test]
    async fn test_ready_to_connecting_via_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let _reconnecting = ready.reconnect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }

    // --- KeyedFutures integration ---

    #[tokio::test]
    async fn test_connecting_in_keyed_futures() {
        let (tx, rx) = tokio::sync::oneshot::channel::<MockService>();
        let connecting =
            ConnectingChannel::new(Box::pin(async move { rx.await.unwrap() }), test_addr());

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> = KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        tx.send(MockService).unwrap();

        match set.poll_next(&mut noop_cx()) {
            Poll::Ready(Some((addr, _))) => assert_eq!(addr, test_addr()),
            _ => panic!("expected Ready"),
        }
    }

    #[tokio::test]
    async fn test_connecting_cancelled_via_keyed_futures() {
        let connecting =
            ConnectingChannel::new(Box::pin(future::pending::<MockService>()), test_addr());

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> = KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        set.cancel(&test_addr()).unwrap();
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Ready(None)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_ready() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: false,
            },
            connector,
        );

        let mut set: KeyedFutures<EndpointAddress, UnejectedChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), ejected).unwrap();

        let (addr, result) = futures_util::future::poll_fn(|cx| set.poll_next(cx))
            .await
            .unwrap();
        assert_eq!(addr, test_addr());
        assert!(matches!(result, UnejectedChannel::Ready(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_needs_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr())
            .connect(connector.clone())
            .await;
        let ejected = ready.eject(
            EjectionConfig {
                timeout: Duration::from_secs(5),
                needs_reconnect: true,
            },
            connector.clone(),
        );

        let mut set: KeyedFutures<EndpointAddress, UnejectedChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), ejected).unwrap();

        let (addr, result) = futures_util::future::poll_fn(|cx| set.poll_next(cx))
            .await
            .unwrap();
        assert_eq!(addr, test_addr());
        assert!(matches!(result, UnejectedChannel::Connecting(_)));
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }
}
