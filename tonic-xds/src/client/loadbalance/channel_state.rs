//! Type-state wrappers for LbChannel lifecycle management.
//!
//! Each state is a separate struct, and transitions consume the old state (move semantics).
//! This prevents using a channel in an invalid state at compile time.
//!
//! ```text
//!                +---reconnect---+
//!                |               |
//!                v               |
//! Idle --> Connecting --> Ready --+--eject--> Ejected
//!                ^                               |
//!                +----------reconnect------------+
//! ```
//!
//! State changes are all one-shot: [`ConnectingChannel`] and [`EjectedChannel`] are
//! [`Future`]s, not streams. The caller (typically a pool) uses [`KeyedFutures`] to
//! manage multiple in-flight state changes and handle cancellation by key.
//!
//! [`KeyedFutures`]: crate::client::loadbalance::keyed_futures::KeyedFutures

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

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
/// Implements [`Future`] -- resolves to `ReadyChannel` when the connection
/// is established. Cancellation is handled externally by the caller
/// (e.g., via [`KeyedFutures::cancel`]).
///
/// [`KeyedFutures::cancel`]: crate::client::loadbalance::keyed_futures::KeyedFutures::cancel
pub(crate) struct ConnectingChannel<S> {
    inner: Pin<Box<dyn Future<Output = ReadyChannel<S>> + Send>>,
}

impl<S: Send + 'static> ConnectingChannel<S> {
    pub(crate) fn new(fut: BoxFuture<S>, addr: EndpointAddress) -> Self {
        Self {
            inner: Box::pin(async move {
                let svc = fut.await;
                ReadyChannel {
                    channel: LbChannel::new(addr, svc),
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
/// Wraps an [`LbChannel`] and delegates [`Service`] and [`Load`] to it.
/// State transitions consume `self` to prevent use-after-transition.
pub(crate) struct ReadyChannel<S> {
    pub(super) channel: LbChannel<S>,
}

impl<S> ReadyChannel<S> {
    /// Eject this channel (e.g., due to outlier detection). Consumes self.
    pub(crate) fn eject<C>(self, config: EjectionConfig, connector: Arc<C>) -> EjectedChannel<S>
    where
        C: Connector<Service = S> + Send + Sync + 'static,
    {
        let ejection_timer = Box::pin(tokio::time::sleep(config.timeout));
        EjectedChannel {
            channel: self.channel,
            config,
            connector,
            ejection_timer,
        }
    }

    /// Start reconnecting this channel. Consumes self, dropping the old connection.
    pub(crate) fn reconnect<C: Connector<Service = S>>(
        self,
        connector: Arc<C>,
    ) -> ConnectingChannel<S>
    where
        S: Send + 'static,
    {
        let addr = self.channel.addr().clone();
        ConnectingChannel::new(connector.connect(&addr), addr)
    }
}

impl<S, Req> Service<Req> for ReadyChannel<S>
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

impl<S> Load for ReadyChannel<S> {
    type Metric = u64;

    fn load(&self) -> Self::Metric {
        self.channel.load()
    }
}

// ---------------------------------------------------------------------------
// EjectedChannel
// ---------------------------------------------------------------------------

/// A channel that has been ejected and is cooling down.
///
/// The underlying connection is kept alive but cannot serve requests.
/// Implements [`Future`] -- resolves once the ejection timer expires to either:
/// - [`UnejectedChannel::Ready`] if no reconnect is needed (clones the channel)
/// - [`UnejectedChannel::Connecting`] if a fresh connection is required
///
/// Cancellation is handled externally by the caller via [`KeyedFutures::cancel`].
///
/// [`KeyedFutures::cancel`]: crate::client::loadbalance::keyed_futures::KeyedFutures::cancel
pub(crate) struct EjectedChannel<S> {
    channel: LbChannel<S>,
    config: EjectionConfig,
    /// `Send + Sync` bounds ensure `EjectedChannel<S>` is `Send + Sync` when `S` is,
    /// enabling use with `KeyedFutures` across async task boundaries.
    connector: Arc<dyn Connector<Service = S> + Send + Sync>,
    ejection_timer: Pin<Box<tokio::time::Sleep>>,
}

impl<S: Clone + Unpin + Send + 'static> Future for EjectedChannel<S> {
    type Output = UnejectedChannel<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.ejection_timer.as_mut().poll(cx) {
            Poll::Ready(()) => {
                if this.config.needs_reconnect {
                    let addr = this.channel.addr().clone();
                    let fut = this.connector.connect(&addr);
                    Poll::Ready(UnejectedChannel::Connecting(ConnectingChannel::new(
                        fut, addr,
                    )))
                } else {
                    let channel = this.channel.clone();
                    Poll::Ready(UnejectedChannel::Ready(ReadyChannel { channel }))
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
        assert_eq!(ready.channel.addr(), &test_addr());
    }

    #[tokio::test]
    async fn test_ready_service_delegates() {
        let connector = MockConnector::new();
        let mut ready = IdleChannel::new(test_addr()).connect(connector).await;
        assert_eq!(ready.call("hello").await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_ready_to_connecting_via_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr()).connect(connector.clone()).await;
        let _reconnecting = ready.reconnect(connector.clone());
        assert_eq!(connector.connect_count.load(Ordering::SeqCst), 2);
    }

    // --- KeyedFutures integration ---

    #[tokio::test]
    async fn test_connecting_in_keyed_futures() {
        let (tx, rx) = tokio::sync::oneshot::channel::<MockService>();
        let connecting =
            ConnectingChannel::new(Box::pin(async move { rx.await.unwrap() }), test_addr());

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        // Before send: pending.
        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        tx.send(MockService).unwrap();

        // After send: ready.
        match set.poll_next(&mut noop_cx()) {
            Poll::Ready(Some((addr, _))) => assert_eq!(addr, test_addr()),
            _ => panic!("expected Ready"),
        }
    }

    #[tokio::test]
    async fn test_connecting_cancelled_via_keyed_futures() {
        let connecting = ConnectingChannel::new(
            Box::pin(future::pending::<MockService>()),
            test_addr(),
        );

        let mut set: KeyedFutures<EndpointAddress, ReadyChannel<MockService>> =
            KeyedFutures::new();
        set.add(test_addr(), connecting).unwrap();

        assert!(matches!(set.poll_next(&mut noop_cx()), Poll::Pending));

        set.cancel(&test_addr()).unwrap();
        for _ in 0..10 {
            match set.poll_next(&mut noop_cx()) {
                Poll::Ready(None) => return,
                _ => tokio::task::yield_now().await,
            }
        }
        panic!("expected set to be empty after cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_ready() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr()).connect(connector.clone()).await;
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

        // Drive via poll_fn so the tokio timer waker is registered properly.
        let (addr, result) = futures_util::future::poll_fn(|cx| set.poll_next(cx))
            .await
            .unwrap();
        assert_eq!(addr, test_addr());
        assert!(matches!(result, UnejectedChannel::Ready(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ejected_in_keyed_futures_needs_reconnect() {
        let connector = MockConnector::new();
        let ready = IdleChannel::new(test_addr()).connect(connector.clone()).await;
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
