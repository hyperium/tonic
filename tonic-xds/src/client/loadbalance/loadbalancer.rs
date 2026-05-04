//! Load balancer tower service.
//!
//! Receives endpoint updates via [`tower::discover::Discover`] (yielding
//! [`IdleChannel`]s), manages the connection lifecycle via the channel state
//! machine, and routes requests to ready endpoints via a [`ChannelPicker`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use indexmap::IndexMap;
use tower::Service;
use tower::discover::{Change, Discover};

use crate::client::endpoint::{Connector, EndpointAddress};
use crate::client::loadbalance::channel_state::{IdleChannel, ReadyChannel};
use crate::client::loadbalance::errors::LbError;
use crate::client::loadbalance::keyed_futures::KeyedFutures;
use crate::client::loadbalance::pickers::ChannelPicker;

/// Future returned by [`LoadBalancer::call`].
///
/// Either resolves immediately with an [`LbError`], or drives `poll_ready` +
/// `call` on the selected channel asynchronously.
pub(crate) enum LbFuture<Resp> {
    Error(Option<LbError>),
    Pending(Pin<Box<dyn Future<Output = Result<Resp, LbError>> + Send>>),
}

impl<Resp> Future for LbFuture<Resp> {
    type Output = Result<Resp, LbError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            LbFuture::Error(error) => match error.take() {
                Some(e) => Poll::Ready(Err(e)),
                None => Poll::Ready(Err(LbError::FailedPrecondition(
                    "LbFuture::Error polled twice",
                ))),
            },
            LbFuture::Pending(fut) => fut.as_mut().poll(cx),
        }
    }
}

/// A load-balancing tower [`Service`] that manages endpoint lifecycle and
/// distributes requests across ready endpoints.
///
/// Type parameters:
/// - `D`: Discovery stream yielding `Change<EndpointAddress, IdleChannel>`
/// - `C`: Connector that produces services from endpoint addresses.
///   `C::Service` is the underlying service type held in ready channels.
/// - `Req`: The request type.
pub(crate) struct LoadBalancer<D, C: Connector, Req> {
    /// Discovery stream providing endpoint additions/removals.
    discovery: D,
    /// Connector for creating connections from idle channels.
    connector: Arc<C>,
    /// In-flight connection attempts, keyed by endpoint address.
    connecting: KeyedFutures<EndpointAddress, ReadyChannel<C::Service>>,
    /// Ready-to-serve channels, keyed by endpoint address.
    ready: IndexMap<EndpointAddress, ReadyChannel<C::Service>>,
    /// Channel picker for load balancing.
    picker: Arc<dyn ChannelPicker<ReadyChannel<C::Service>, Req> + Send + Sync>,
}

impl<D, C, Req> LoadBalancer<D, C, Req>
where
    D: Discover<Key = EndpointAddress, Service = IdleChannel> + Unpin,
    D::Error: Into<tower::BoxError>,
    C: Connector + Send + Sync + 'static,
    C::Service: Send + 'static,
{
    /// Create a new load balancer with the given picker.
    pub(crate) fn new(
        discovery: D,
        connector: Arc<C>,
        picker: Arc<dyn ChannelPicker<ReadyChannel<C::Service>, Req> + Send + Sync>,
    ) -> Self {
        Self {
            discovery,
            connector,
            connecting: KeyedFutures::new(),
            ready: IndexMap::new(),
            picker,
        }
    }

    /// Drain pending discovery events. Either resolves to an error
    /// ([`LbError::DiscoverClosed`] or [`LbError::DiscoverError`]) or stays
    /// pending — there is no success outcome since the loop only exits on
    /// pending or error.
    fn poll_discover(&mut self, cx: &mut Context<'_>) -> Poll<LbError> {
        loop {
            match ready!(Pin::new(&mut self.discovery).poll_discover(cx)) {
                None => {
                    // tower::discover::Discover::poll_discover() returns Ready(None) when the
                    // discover object is closed, as indicated by Stream trait.
                    tracing::error!("discover object is closed");
                    return Poll::Ready(LbError::DiscoverClosed);
                }
                Some(Err(e)) => return Poll::Ready(LbError::DiscoverError(e.into())),
                Some(Ok(Change::Insert(addr, idle))) => {
                    tracing::trace!("discovery: insert {addr}");
                    let _ = self.connecting.cancel(&addr);
                    self.ready.swap_remove(&addr);
                    let connecting = idle.connect(self.connector.clone());
                    let _ = self.connecting.add(addr, connecting);
                }
                Some(Ok(Change::Remove(addr))) => {
                    tracing::trace!("discovery: remove {addr}");
                    let _ = self.connecting.cancel(&addr);
                    self.ready.swap_remove(&addr);
                }
            }
        }
    }

    /// Drain completed connection futures into the ready set.
    fn poll_connecting(&mut self, cx: &mut Context<'_>) {
        while let Poll::Ready(Some((addr, ready))) = self.connecting.poll_next(cx) {
            self.ready.insert(addr, ready);
        }
    }
}

impl<D, C, Req> Service<Req> for LoadBalancer<D, C, Req>
where
    D: Discover<Key = EndpointAddress, Service = IdleChannel> + Unpin,
    D::Error: Into<tower::BoxError>,
    C: Connector + Send + Sync + 'static,
    C::Service: Service<Req> + Clone + Send + 'static,
    <C::Service as Service<Req>>::Response: Send + 'static,
    <C::Service as Service<Req>>::Error: Into<tower::BoxError>,
    <C::Service as Service<Req>>::Future: Send + 'static,
    Req: Send + 'static,
{
    type Response = <C::Service as Service<Req>>::Response;
    type Error = LbError;
    type Future = LbFuture<Self::Response>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let discover_result = self.poll_discover(cx);
        self.poll_connecting(cx);

        if !self.ready.is_empty() {
            return Poll::Ready(Ok(()));
        }

        // No ready endpoints. Check if we should fail fast.
        match discover_result {
            Poll::Ready(LbError::DiscoverClosed) if self.connecting.len() == 0 => {
                // Discovery is closed and nothing is connecting — no progress is possible.
                Poll::Ready(Err(LbError::Stagnation))
            }
            Poll::Ready(e) => {
                // Other discovery errors (or DiscoverClosed with connecting in flight)
                // are non-fatal — log and stay pending.
                tracing::warn!("discovery yielded error: {e}");
                Poll::Pending
            }
            Poll::Pending => {
                tracing::trace!(
                    "waiting for connections, inflight={}",
                    self.connecting.len()
                );
                Poll::Pending
            }
        }
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let Some(picked) = self.picker.pick(&req, &self.ready) else {
            return LbFuture::Error(Some(LbError::Unavailable));
        };
        // `picked` is a read-only borrow into `self.ready`. Clone to get an
        // owned service we can drive in the async block.
        let mut svc = picked.clone();
        LbFuture::Pending(Box::pin(async move {
            tower::ServiceExt::ready(&mut svc)
                .await
                .map_err(|e| LbError::LbChannelPollReadyError(e.into()))?;
            svc.call(req)
                .await
                .map_err(|e| LbError::LbChannelCallError(e.into()))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::Connector;
    use crate::client::loadbalance::pickers::p2c::P2cPicker;
    use crate::common::async_util::BoxFuture;
    use futures_util::FutureExt;
    use std::future;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;
    use tower::load::Load;

    // -- Mock service --

    use std::sync::atomic::AtomicBool;

    #[derive(Clone)]
    struct MockService {
        load: Arc<AtomicU64>,
        /// When true, poll_ready returns an error.
        fail_poll_ready: Arc<AtomicBool>,
        /// When true, call returns an error.
        fail_call: Arc<AtomicBool>,
        /// Tracks how many times call() was invoked.
        call_count: Arc<AtomicU64>,
    }

    impl MockService {
        fn new() -> Self {
            Self {
                load: Arc::new(AtomicU64::new(0)),
                fail_poll_ready: Arc::new(AtomicBool::new(false)),
                fail_call: Arc::new(AtomicBool::new(false)),
                call_count: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl Service<&'static str> for MockService {
        type Response = &'static str;
        type Error = tower::BoxError;
        type Future = future::Ready<Result<&'static str, tower::BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if self.fail_poll_ready.load(Ordering::Relaxed) {
                Poll::Ready(Err("injected poll_ready error".into()))
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn call(&mut self, _req: &'static str) -> Self::Future {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            if self.fail_call.load(Ordering::Relaxed) {
                future::ready(Err("injected call error".into()))
            } else {
                future::ready(Ok("ok"))
            }
        }
    }

    impl Load for MockService {
        type Metric = u64;
        fn load(&self) -> Self::Metric {
            self.load.load(Ordering::Relaxed)
        }
    }

    // -- Mock connector --

    /// A connector that returns a pending future until signaled via oneshot.
    /// Each `connect()` call stores the sender so tests can control when
    /// connections complete.
    use std::collections::HashMap;

    struct MockConnector {
        senders:
            std::sync::Mutex<HashMap<EndpointAddress, tokio::sync::oneshot::Sender<MockService>>>,
        /// Services created by resolve_all, keyed by endpoint address.
        services: std::sync::Mutex<HashMap<EndpointAddress, MockService>>,
    }

    impl MockConnector {
        fn new() -> Self {
            Self {
                senders: std::sync::Mutex::new(HashMap::new()),
                services: std::sync::Mutex::new(HashMap::new()),
            }
        }

        /// Complete all pending connections.
        fn resolve_all(&self) {
            let senders: HashMap<_, _> = self.senders.lock().unwrap().drain().collect();
            for (addr, tx) in senders {
                let svc = MockService::new();
                self.services.lock().unwrap().insert(addr, svc.clone());
                let _ = tx.send(svc);
            }
        }

        /// Get the service for the given address (created by resolve_all).
        fn service(&self, addr: &EndpointAddress) -> MockService {
            self.services.lock().unwrap()[addr].clone()
        }

        /// Number of pending (unresolved) connections.
        fn pending_count(&self) -> usize {
            self.senders.lock().unwrap().len()
        }
    }

    impl Connector for MockConnector {
        type Service = MockService;

        fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.senders.lock().unwrap().insert(addr.clone(), tx);
            Box::pin(async move { rx.await.unwrap() })
        }
    }

    // -- Discovery helper --

    type DiscoverItem = Result<Change<EndpointAddress, IdleChannel>, tower::BoxError>;

    /// Tower's `Discover` is sealed, but has a blanket impl for any
    /// `Stream<Item = Result<Change<K, S>, E>>`. We use `ReceiverStream` directly.
    type MockDiscover = tokio_stream::wrappers::ReceiverStream<DiscoverItem>;

    fn addr(port: u16) -> EndpointAddress {
        EndpointAddress::new("127.0.0.1", port)
    }

    fn new_discover() -> (mpsc::Sender<DiscoverItem>, MockDiscover) {
        let (tx, rx) = mpsc::channel(16);
        (tx, tokio_stream::wrappers::ReceiverStream::new(rx))
    }

    fn make_lb(
        discover: MockDiscover,
    ) -> (
        LoadBalancer<MockDiscover, MockConnector, &'static str>,
        Arc<MockConnector>,
    ) {
        let connector = Arc::new(MockConnector::new());
        let picker: Arc<dyn ChannelPicker<ReadyChannel<MockService>, &'static str> + Send + Sync> =
            Arc::new(P2cPicker);
        let lb = LoadBalancer::new(discover, connector.clone(), picker);
        (lb, connector)
    }

    type Lb = LoadBalancer<MockDiscover, MockConnector, &'static str>;

    /// Poll poll_ready once synchronously. Returns `Some(Ok(()))` if ready,
    /// `Some(Err(_))` on error, `None` if pending.
    fn poll_ready_now<L: Service<&'static str, Error = LbError>>(
        lb: &mut L,
    ) -> Option<Result<(), LbError>> {
        futures_util::future::poll_fn(|cx| lb.poll_ready(cx)).now_or_never()
    }

    /// Drive poll_ready until the LB has ready endpoints.
    async fn drive_to_ready(lb: &mut Lb, connector: &Arc<MockConnector>) {
        let c = connector.clone();
        tokio::spawn(async move { c.resolve_all() });
        futures_util::future::poll_fn(|cx| lb.poll_ready(cx))
            .await
            .unwrap();
    }

    // -- poll_discover tests --

    #[tokio::test]
    async fn test_discover_insert_starts_connecting() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();

        // Discovers insert, starts connecting, returns Pending (no ready yet).
        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(connector.pending_count(), 1);
        assert_eq!(lb.ready.len(), 0);
    }

    #[tokio::test]
    async fn test_discover_insert_then_resolves_to_ready() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();

        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(connector.pending_count(), 1);

        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 1);
        assert!(lb.ready.contains_key(&addr(8080)));
    }

    #[tokio::test]
    async fn test_discover_remove_cancels_connecting() {
        let (tx, discover) = new_discover();
        let (mut lb, _connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        tx.send(Ok(Change::Remove(addr(8080)))).await.unwrap();

        // Both processed in one poll — insert then remove cancels the connecting.
        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(lb.ready.len(), 0);
        assert_eq!(lb.connecting.len(), 0);
    }

    #[tokio::test]
    async fn test_discover_remove_evicts_ready() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 1);

        tx.send(Ok(Change::Remove(addr(8080)))).await.unwrap();
        // After remove, ready is empty → Pending.
        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(lb.ready.len(), 0);
    }

    #[tokio::test]
    async fn test_discover_replace_endpoint() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 1);

        // Re-insert same address — old ready evicted, new one connecting.
        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(lb.ready.len(), 0);
        assert_eq!(connector.pending_count(), 1);

        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 1);
    }

    #[tokio::test]
    async fn test_discover_multiple_endpoints() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        for port in 8080..8083 {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }

        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(connector.pending_count(), 3);
        assert_eq!(lb.ready.len(), 0);

        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 3);
    }

    #[tokio::test]
    async fn test_discover_remove_nonexistent_is_defensive() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 1);

        // Remove an address that was never added — should not crash or affect existing.
        tx.send(Ok(Change::Remove(addr(9999)))).await.unwrap();
        // Still has one ready endpoint → Ready.
        poll_ready_now(&mut lb).unwrap().unwrap();
        assert_eq!(lb.ready.len(), 1);
        assert!(lb.ready.contains_key(&addr(8080)));
    }

    /// When discovery is closed, poll_ready behaves based on connecting/ready state:
    /// - ready.len() > 0 → Ready(Ok(()))
    /// - ready.len() == 0 && connecting.len() == 0 → Pending forever
    /// - ready.len() == 0 && connecting.len() > 0 → Pending, but wakes when connecting resolves
    #[tokio::test]
    async fn test_poll_ready_with_closed_discovery() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        // Send an Insert and close the discovery stream.
        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drop(tx);

        // poll_discover drains the Insert, then sees Ready(None) (closed) → returns Ready(Ok(())).
        // ready=0, connecting=1 → Pending. Connecting waker is registered.
        assert!(poll_ready_now(&mut lb).is_none());
        assert_eq!(lb.connecting.len(), 1);
        assert_eq!(lb.ready.len(), 0);

        // Resolve the connection synchronously.
        connector.resolve_all();

        // Now ready.len() > 0 → poll_ready returns Ready(Ok(())).
        let result = poll_ready_now(&mut lb);
        assert!(
            matches!(result, Some(Ok(()))),
            "expected Ready(Ok(())), got {result:?}"
        );
        assert_eq!(lb.ready.len(), 1);
    }

    /// When discovery is closed and there are no connecting futures or ready
    /// endpoints, poll_ready fails fast with Stagnation rather than hanging.
    #[tokio::test]
    async fn test_poll_ready_stagnation_when_closed_and_empty() {
        let (tx, discover) = new_discover();
        let (mut lb, _connector) = make_lb(discover);

        // Close discovery without sending any Insert.
        drop(tx);

        // poll_discover returns Err(DiscoverClosed); ready=0 and connecting=0
        // → poll_ready fails fast with Stagnation.
        let result = poll_ready_now(&mut lb).unwrap();
        assert!(
            matches!(result, Err(LbError::Stagnation)),
            "expected Stagnation, got {result:?}"
        );
        assert_eq!(lb.connecting.len(), 0);
        assert_eq!(lb.ready.len(), 0);
    }

    // -- call() tests --

    #[tokio::test]
    async fn test_call_no_endpoints_returns_unavailable() {
        let (_tx, discover) = new_discover();
        let (mut lb, _connector) = make_lb(discover);

        // poll_ready returns Pending — calling anyway violates tower contract,
        // but the picker returns None so call returns Unavailable.
        assert!(poll_ready_now(&mut lb).is_none());

        let result = lb.call("hello").await;
        assert!(
            matches!(result, Err(LbError::Unavailable)),
            "expected Unavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_call_success() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;

        let result = lb.call("hello").await;
        assert_eq!(result.unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_call_distributes_across_endpoints() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        let ports: Vec<u16> = (8080..8085).collect();
        for &port in &ports {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 5);

        let num_requests = 1000;
        for _ in 0..num_requests {
            assert_eq!(lb.call("hello").await.unwrap(), "ok");
        }

        // Check all endpoints were called.
        let mut total = 0u64;
        for &port in &ports {
            let svc = connector.service(&addr(port));
            let count = svc.call_count.load(Ordering::Relaxed);
            assert!(count > 0, "endpoint {port} was never called");
            total += count;
        }
        assert_eq!(total, num_requests);

        // Check distribution is reasonably balanced (within 3x of uniform).
        let expected = num_requests / ports.len() as u64;
        for &port in &ports {
            let svc = connector.service(&addr(port));
            let count = svc.call_count.load(Ordering::Relaxed);
            assert!(
                count >= expected / 3 && count <= expected * 3,
                "endpoint {port} got {count} calls, expected ~{expected}"
            );
        }
    }

    #[tokio::test]
    async fn test_call_channel_poll_ready_error() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;

        // Inject poll_ready failure.
        connector
            .service(&addr(8080))
            .fail_poll_ready
            .store(true, Ordering::Relaxed);

        let result = lb.call("hello").await;
        assert!(
            matches!(result, Err(LbError::LbChannelPollReadyError(_))),
            "expected LbChannelPollReadyError, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_call_channel_call_error() {
        let (tx, discover) = new_discover();
        let (mut lb, connector) = make_lb(discover);

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;

        // Inject call failure.
        connector
            .service(&addr(8080))
            .fail_call
            .store(true, Ordering::Relaxed);

        let result = lb.call("hello").await;
        assert!(
            matches!(result, Err(LbError::LbChannelCallError(_))),
            "expected LbChannelCallError, got {result:?}"
        );
    }
}
