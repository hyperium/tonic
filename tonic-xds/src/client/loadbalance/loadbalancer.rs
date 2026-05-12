//! Load balancer tower service.
//!
//! Receives endpoint updates via [`tower::discover::Discover`],
//! manages the connection lifecycle via the channel state machine,
//! and routes requests to ready endpoints via a [`ChannelPicker`].
//!
//! Outlier detection (gRFC A50) is integrated via an optional
//! [`OutlierDetector`]. Eject requests arrive on an mpsc channel from
//! the data path; the LB consumes the matching [`ReadyChannel`] via
//! [`ReadyChannel::eject`] and tracks the resulting
//! [`EjectedChannel`] in [`Self::ejected`]. When the timer fires, the
//! resolved [`UnejectedChannel`] is routed back into `ready` or
//! `connecting`.
//!
//! [`EjectedChannel`]: crate::client::loadbalance::channel_state::EjectedChannel
//! [`UnejectedChannel`]: crate::client::loadbalance::channel_state::UnejectedChannel

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use tower::Service;
use tower::discover::{Change, Discover};

use crate::client::endpoint::{Connector, EndpointAddress};
use crate::client::loadbalance::channel_state::{
    EjectionConfig, IdleChannel, OutlierChannelState, ReadyChannel, UnejectedChannel,
};
use crate::client::loadbalance::errors::LbError;
use crate::client::loadbalance::keyed_futures::KeyedFutures;
use crate::client::loadbalance::outlier_detection::{
    OutlierDetector, OutlierStatsRegistry, RegistryAlreadyWired,
};
use crate::client::loadbalance::pickers::ChannelPicker;

/// Future returned by [`LoadBalancer::call`]. Either resolves
/// immediately with an [`LbError`] or drives the selected channel.
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
    discovery: D,
    connector: Arc<C>,
    /// In-flight connection attempts.
    connecting: KeyedFutures<EndpointAddress, C::Service>,
    /// Ready-to-serve channels.
    ready: IndexMap<EndpointAddress, ReadyChannel<C::Service>>,
    /// Currently-ejected channels. Each entry is an
    /// [`EjectedChannel`] whose `Sleep` fires when the ejection
    /// window expires.
    ejected: KeyedFutures<EndpointAddress, UnejectedChannel<C::Service>>,
    /// `None` disables outlier detection.
    outlier: Option<OutlierDetector>,
    picker: Arc<dyn ChannelPicker<ReadyChannel<C::Service>, Req> + Send + Sync>,
}

impl<D, C, Req> LoadBalancer<D, C, Req>
where
    D: Discover<Key = EndpointAddress, Service = IdleChannel> + Unpin,
    D::Error: Into<tower::BoxError>,
    C: Connector + Send + Sync + 'static,
    C::Service: Clone + Send + 'static,
{
    /// Create a load balancer with no outlier detection.
    pub(crate) fn new(
        discovery: D,
        connector: Arc<C>,
        picker: Arc<dyn ChannelPicker<ReadyChannel<C::Service>, Req> + Send + Sync>,
    ) -> Self {
        // Infallible: `with_outlier(.., None)` never wires a registry.
        Self::with_outlier(discovery, connector, picker, None)
            .expect("with_outlier(.., None) is infallible")
    }

    /// Create a load balancer, optionally enabling outlier detection.
    /// When `outlier` is `Some`, the registry's housekeeping actor is
    /// spawned and bound to this LB. Returns
    /// [`RegistryAlreadyWired`] if the registry already drives
    /// another LB.
    pub(crate) fn with_outlier(
        discovery: D,
        connector: Arc<C>,
        picker: Arc<dyn ChannelPicker<ReadyChannel<C::Service>, Req> + Send + Sync>,
        outlier: Option<Arc<OutlierStatsRegistry>>,
    ) -> Result<Self, RegistryAlreadyWired> {
        let outlier = outlier.map(OutlierDetector::new).transpose()?;
        Ok(Self {
            discovery,
            connector,
            connecting: KeyedFutures::new(),
            ready: IndexMap::new(),
            ejected: KeyedFutures::new(),
            outlier,
            picker,
        })
    }

    /// Purge all state for `addr`, including the outlier-detection
    /// registry entry. Called on `Change::Remove`.
    fn purge_endpoint(&mut self, addr: &EndpointAddress) {
        let _ = self.connecting.cancel(addr);
        self.ready.swap_remove(addr);
        let _ = self.ejected.cancel(addr);
        if let Some(o) = self.outlier.as_ref() {
            o.registry().remove_channel(addr);
        }
    }

    /// Clear stale connecting/ready/ejected slots for `addr` but
    /// preserve the outlier-detection registry entry. Called on
    /// `Change::Insert` so transient discovery flaps don't lose
    /// counters or ejection state, matching grpc-go and Envoy.
    fn reset_active_slots(&mut self, addr: &EndpointAddress) {
        let _ = self.connecting.cancel(addr);
        self.ready.swap_remove(addr);
        let _ = self.ejected.cancel(addr);
    }

    /// Drain pending discovery events. Resolves to an error
    /// ([`LbError::DiscoverClosed`] or [`LbError::DiscoverError`])
    /// or stays pending — there is no success outcome.
    fn poll_discover(&mut self, cx: &mut Context<'_>) -> Poll<LbError> {
        loop {
            match ready!(Pin::new(&mut self.discovery).poll_discover(cx)) {
                None => {
                    tracing::error!("discover object is closed");
                    return Poll::Ready(LbError::DiscoverClosed);
                }
                Some(Err(e)) => return Poll::Ready(LbError::DiscoverError(e.into())),
                Some(Ok(Change::Insert(addr, idle))) => {
                    tracing::trace!("discovery: insert {addr}");
                    self.reset_active_slots(&addr);
                    let connecting = idle.connect(self.connector.clone());
                    let _ = self.connecting.add(addr, connecting);
                }
                Some(Ok(Change::Remove(addr))) => {
                    tracing::trace!("discovery: remove {addr}");
                    self.purge_endpoint(&addr);
                }
            }
        }
    }

    /// Drain completed connection futures. If the outlier state for
    /// a re-discovered endpoint is still ejected, the new channel is
    /// re-ejected for the *remaining* duration; if the deadline has
    /// already passed, it is un-ejected and routed to `ready`.
    fn poll_connecting(&mut self, cx: &mut Context<'_>) {
        while let Poll::Ready(Some((addr, svc))) = self.connecting.poll_next(cx) {
            let state = match self.outlier.as_ref() {
                Some(o) => o.registry().add_channel(addr.clone()),
                None => Arc::new(OutlierChannelState::new(addr.clone())),
            };
            let ready = ReadyChannel::new(addr.clone(), svc, state.clone());
            let remaining = self
                .outlier
                .as_ref()
                .and_then(|o| o.registry().remaining_ejection(&state, Instant::now()));
            self.place_after_connect(addr, ready, remaining);
        }
    }

    /// Route a freshly-connected `ReadyChannel` based on its
    /// preserved outlier state: `None` → ready; `Some(0)` → un-eject
    /// then ready; `Some(d)` → ejected for `d`.
    fn place_after_connect(
        &mut self,
        addr: EndpointAddress,
        ready: ReadyChannel<C::Service>,
        remaining: Option<Duration>,
    ) {
        match remaining {
            None => {
                self.ready.insert(addr, ready);
            }
            Some(d) if d.is_zero() => {
                if let Some(o) = self.outlier.as_ref() {
                    o.registry().note_uneject(ready.outlier());
                }
                self.ready.insert(addr, ready);
            }
            Some(d) => {
                let ejected = ready.eject(
                    EjectionConfig {
                        timeout: d,
                        needs_reconnect: false,
                    },
                    self.connector.clone(),
                );
                tracing::debug!("outlier detection: re-eject {addr} for {d:?}");
                let _ = self.ejected.add(addr, ejected);
            }
        }
    }

    /// Drain eject requests from the outlier detector's mpsc and
    /// move each named `ReadyChannel` into [`Self::ejected`]. The
    /// per-channel ejection flag has already been set by
    /// `record_outcome`.
    fn poll_eject_requests(&mut self, cx: &mut Context<'_>) {
        loop {
            let Some(o) = self.outlier.as_mut() else {
                return;
            };
            let addr = match o.poll_eject_request(cx) {
                Poll::Ready(Some(a)) => a,
                _ => return,
            };
            let registry = o.registry().clone();
            // Channel may have been removed by discovery in the
            // meantime; if so, nothing to eject.
            let Some(ch) = self.ready.swap_remove(&addr) else {
                continue;
            };
            let state = ch.outlier().clone();
            match registry.remaining_ejection(&state, Instant::now()) {
                Some(d) if !d.is_zero() => {
                    let ejected = ch.eject(
                        EjectionConfig {
                            timeout: d,
                            needs_reconnect: false,
                        },
                        self.connector.clone(),
                    );
                    tracing::debug!("outlier detection: eject {addr} for {d:?}");
                    let _ = self.ejected.add(addr, ejected);
                }
                Some(_) => {
                    // Deadline already past — un-eject.
                    registry.note_uneject(&state);
                    self.ready.insert(addr, ch);
                }
                None => {
                    // No longer ejected (raced with un-eject).
                    self.ready.insert(addr, ch);
                }
            }
        }
    }

    /// Drain completed `EjectedChannel` timers. Clears the
    /// registry-level ejection counter and routes the resolved
    /// channel back into `ready` (with its outlier state already
    /// reattached) or `connecting`.
    fn poll_unejection(&mut self, cx: &mut Context<'_>) {
        while let Poll::Ready(Some((addr, unejected))) = self.ejected.poll_next(cx) {
            match unejected {
                UnejectedChannel::Ready(ready) => {
                    if let Some(o) = self.outlier.as_ref() {
                        o.registry().note_uneject(ready.outlier());
                    }
                    tracing::debug!("outlier detection: uneject {addr}");
                    self.ready.insert(addr, ready);
                }
                // `needs_reconnect = false` for A50; this arm is
                // reserved for future policies.
                UnejectedChannel::Connecting(future) => {
                    if let Some(o) = self.outlier.as_ref() {
                        let state = o.registry().add_channel(addr.clone());
                        o.registry().note_uneject(&state);
                    }
                    let _ = self.connecting.add(addr, future);
                }
            }
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
        // Un-ejections before ejections so `ejected_count` is current
        // when the next eject is evaluated.
        self.poll_unejection(cx);
        self.poll_connecting(cx);
        self.poll_eject_requests(cx);

        if !self.ready.is_empty() {
            return Poll::Ready(Ok(()));
        }

        // No ready endpoints. Fail fast iff discovery is closed and
        // nothing else can produce one.
        match discover_result {
            Poll::Ready(LbError::DiscoverClosed) if self.connecting.len() == 0 => {
                Poll::Ready(Err(LbError::Stagnation))
            }
            Poll::Ready(e) => {
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
        // Cheap clones (all Arc-shared internals) so the async block
        // can take ownership without holding the picker borrow.
        let mut svc = picked.clone();
        let outlier_state = picked.outlier().clone();
        let registry = self.outlier.as_ref().map(|o| o.registry().clone());
        LbFuture::Pending(Box::pin(async move {
            tower::ServiceExt::ready(&mut svc)
                .await
                .map_err(|e| LbError::LbChannelPollReadyError(e.into()))?;
            let result = svc.call(req).await;
            if let Some(registry) = registry.as_ref() {
                registry.record_outcome(&outlier_state, result.is_ok());
            }
            result.map_err(|e| LbError::LbChannelCallError(e.into()))
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

    // -- Outlier-detection integration tests --

    use crate::client::loadbalance::outlier_detection::{OutlierStatsRegistry, Rng};
    use crate::xds::resource::outlier_detection::{
        FailurePercentageConfig, OutlierDetectionConfig, Percentage,
    };
    use std::time::Duration;

    fn pct(v: u32) -> Percentage {
        Percentage::new(v).unwrap()
    }

    struct AlwaysFireRng;
    impl Rng for AlwaysFireRng {
        fn pct_roll(&self) -> u32 {
            0
        }
    }

    fn fp_config(
        threshold: u32,
        request_volume: u32,
        minimum_hosts: u32,
    ) -> OutlierDetectionConfig {
        OutlierDetectionConfig {
            interval: Duration::from_secs(60),
            base_ejection_time: Duration::from_secs(30),
            max_ejection_time: Duration::from_secs(300),
            max_ejection_percent: pct(100),
            success_rate: None,
            failure_percentage: Some(FailurePercentageConfig {
                threshold: pct(threshold),
                enforcing_failure_percentage: pct(100),
                minimum_hosts,
                request_volume,
            }),
        }
    }

    /// Build an LB with outlier detection enabled.
    fn make_lb_with_outlier(
        discover: MockDiscover,
        config: OutlierDetectionConfig,
    ) -> (Lb, Arc<MockConnector>, Arc<OutlierStatsRegistry>) {
        let connector = Arc::new(MockConnector::new());
        let picker: Arc<dyn ChannelPicker<ReadyChannel<MockService>, &'static str> + Send + Sync> =
            Arc::new(P2cPicker);
        let registry = OutlierStatsRegistry::with_rng(config, Box::new(AlwaysFireRng));
        let lb =
            LoadBalancer::with_outlier(discover, connector.clone(), picker, Some(registry.clone()))
                .expect("registry not yet wired");
        (lb, connector, registry)
    }

    /// Drive the LB through one call per port. Asserts each succeeds.
    async fn call_each(lb: &mut Lb, n: usize) {
        for _ in 0..n {
            lb.call("hello").await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_outlier_detection_ejects_failing_endpoint() {
        // 5 endpoints, all healthy except 8084. Once 8084's failures
        // cross the threshold, it should be moved out of `ready` and
        // into `ejected`.
        let (tx, discover) = new_discover();
        let (mut lb, connector, registry) = make_lb_with_outlier(
            discover,
            fp_config(
                /*threshold*/ 50, /*request_volume*/ 5, /*minimum_hosts*/ 3,
            ),
        );

        for port in 8080..=8084 {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 5);

        // Configure 8084 to always fail. Other endpoints stay healthy.
        connector
            .service(&addr(8084))
            .fail_call
            .store(true, Ordering::Relaxed);

        // Drive enough calls to ensure 8084 reaches request_volume
        // and its failure rate triggers ejection. With 5 endpoints
        // and P2C picking, each gets ~k/5 calls; drive 100 to be safe.
        for _ in 0..100 {
            let _ = lb.call("hello").await;
        }

        // poll_ready drains the eject mpsc and transitions 8084 into
        // `self.ejected` via `ReadyChannel::eject`.
        let _ = poll_ready_now(&mut lb);
        assert!(
            lb.ejected.contains_key(&addr(8084)),
            "8084 should be ejected; ejected.len()={}, ready keys: {:?}",
            lb.ejected.len(),
            lb.ready.keys().collect::<Vec<_>>(),
        );
        assert!(!lb.ready.contains_key(&addr(8084)));
        // The registry's `ejected_count` should reflect the same.
        assert!(registry.len() == 5);
    }

    #[tokio::test]
    async fn test_outlier_detection_healthy_cluster_no_ejections() {
        let (tx, discover) = new_discover();
        let (mut lb, connector, _registry) = make_lb_with_outlier(discover, fp_config(50, 5, 3));

        for port in 8080..=8084 {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(lb.ready.len(), 5);

        call_each(&mut lb, 50).await;

        let _ = poll_ready_now(&mut lb);
        assert_eq!(lb.ejected.len(), 0);
        assert_eq!(lb.ready.len(), 5);
    }

    #[tokio::test]
    async fn test_outlier_detection_endpoint_removal_cleans_registry() {
        let (tx, discover) = new_discover();
        let (mut lb, connector, registry) = make_lb_with_outlier(discover, fp_config(50, 5, 3));

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;
        assert_eq!(registry.len(), 1);

        tx.send(Ok(Change::Remove(addr(8080)))).await.unwrap();
        let _ = poll_ready_now(&mut lb);
        assert_eq!(registry.len(), 0);
        assert_eq!(lb.ready.len(), 0);
    }

    /// Re-discovering an endpoint (Insert for an address the LB
    /// already tracks) must preserve its outlier-detection counters
    /// and multiplier. Matches grpc-go / Envoy behavior.
    #[tokio::test]
    async fn test_outlier_detection_reinsert_preserves_state() {
        let (tx, discover) = new_discover();
        let (mut lb, connector, registry) = make_lb_with_outlier(discover, fp_config(50, 5, 3));

        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;
        let state = registry.add_channel(addr(8080)); // idempotent — returns the existing state
        // Drive some successes through the data path so the channel
        // accumulates counter state worth preserving.
        for _ in 0..3 {
            lb.call("hello").await.unwrap();
        }
        let (s_before, f_before) = state.counters();
        assert!(
            s_before > 0,
            "expected accumulated successes before re-insert"
        );
        let registry_before = Arc::as_ptr(&state);

        // Re-insert the same address. State must survive.
        tx.send(Ok(Change::Insert(addr(8080), IdleChannel::new(addr(8080)))))
            .await
            .unwrap();
        drive_to_ready(&mut lb, &connector).await;

        let state_after = registry.add_channel(addr(8080));
        assert_eq!(
            Arc::as_ptr(&state_after),
            registry_before,
            "registry entry should be the same Arc — state continuity preserved",
        );
        let (s_after, f_after) = state_after.counters();
        assert_eq!(
            (s_after, f_after),
            (s_before, f_before),
            "counters must survive re-insert",
        );
        assert_eq!(registry.len(), 1);
    }

    /// A re-discovered endpoint whose preserved state says "ejected"
    /// is placed directly into the ejected pool, not the ready set, so
    /// no traffic is routed to it until the housekeeping actor
    /// un-ejects it.
    #[tokio::test]
    async fn test_outlier_detection_reinsert_while_ejected_stays_ejected() {
        let (tx, discover) = new_discover();
        let (mut lb, connector, registry) = make_lb_with_outlier(discover, fp_config(50, 5, 3));

        // Bring up 5 endpoints; make 8084 fail enough to be ejected.
        for port in 8080..=8084 {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }
        drive_to_ready(&mut lb, &connector).await;
        connector
            .service(&addr(8084))
            .fail_call
            .store(true, Ordering::Relaxed);
        for _ in 0..100 {
            let _ = lb.call("hello").await;
        }
        let _ = poll_ready_now(&mut lb);
        let state_8084 = registry.add_channel(addr(8084));
        assert!(
            state_8084.is_ejected(),
            "8084 must be ejected before re-insert"
        );
        assert!(
            lb.ejected.contains_key(&addr(8084)),
            "8084 should be in the ejected pool"
        );

        // Re-insert 8084. The ejected slot's old EjectedChannel is
        // cancelled, but the registry entry (is_ejected=true,
        // ejected_at_nanos preserved) survives. The new channel
        // should be re-ejected with the *remaining* ejection time.
        // Drive the steps explicitly because `lb.ready` is non-empty
        // throughout (8080..=8083), so `drive_to_ready` may return
        // before the new 8084 connect resolves.
        tx.send(Ok(Change::Insert(addr(8084), IdleChannel::new(addr(8084)))))
            .await
            .unwrap();
        // 1. Drain the Insert into `self.connecting`.
        let _ = poll_ready_now(&mut lb);
        // 2. Synchronously resolve the new connect future.
        connector.resolve_all();
        // 3. Drain the now-ready connecting future; `poll_connecting`
        //    sees `state.is_ejected() == true` and re-ejects.
        let _ = poll_ready_now(&mut lb);

        assert!(
            !lb.ready.contains_key(&addr(8084)),
            "8084 must not be in ready while still logically ejected"
        );
        assert!(
            lb.ejected.contains_key(&addr(8084)),
            "8084 must remain in the ejected pool after re-insert"
        );
        assert!(state_8084.is_ejected());
    }

    /// Once `base × multiplier` time elapses on an ejected channel,
    /// the [`EjectedChannel`]'s timer fires and the LB's
    /// `poll_unejection` should move the channel back to `ready`.
    #[tokio::test(start_paused = true)]
    async fn test_outlier_detection_timer_driven_unejection() {
        let mut config = fp_config(50, 5, 3);
        // Short base for fast test; multiplier is 1 on first eject.
        config.base_ejection_time = Duration::from_secs(10);
        config.max_ejection_time = Duration::from_secs(60);

        let (tx, discover) = new_discover();
        let (mut lb, connector, registry) = make_lb_with_outlier(discover, config);

        for port in 8080..=8084 {
            tx.send(Ok(Change::Insert(addr(port), IdleChannel::new(addr(port)))))
                .await
                .unwrap();
        }
        drive_to_ready(&mut lb, &connector).await;
        connector
            .service(&addr(8084))
            .fail_call
            .store(true, Ordering::Relaxed);
        for _ in 0..100 {
            let _ = lb.call("hello").await;
        }
        let _ = poll_ready_now(&mut lb);
        assert!(
            lb.ejected.contains_key(&addr(8084)),
            "8084 must be ejected before the timer fires"
        );
        assert!(registry.add_channel(addr(8084)).is_ejected());

        // Stop 8084 from failing so it can serve again, then advance
        // past `base × multiplier = 10s`.
        connector
            .service(&addr(8084))
            .fail_call
            .store(false, Ordering::Relaxed);
        tokio::time::advance(Duration::from_secs(11)).await;
        // Drive poll_ready; `EjectedChannel`'s timer fires and
        // `poll_unejection` routes 8084 back to ready.
        let _ = poll_ready_now(&mut lb);

        assert!(
            !lb.ejected.contains_key(&addr(8084)),
            "8084 must leave the ejected pool once the timer fires"
        );
        assert!(
            lb.ready.contains_key(&addr(8084)),
            "8084 must be back in ready after un-ejection"
        );
        assert!(!registry.add_channel(addr(8084)).is_ejected());
    }

    /// Sharing one `OutlierStatsRegistry` across two `LoadBalancer`s is
    /// not supported — the eject-signal receiver is one-shot. The
    /// second `with_outlier` call must return an error rather than
    /// panic.
    #[tokio::test]
    async fn test_outlier_registry_cannot_be_wired_twice() {
        let (_tx1, discover1) = new_discover();
        let (_tx2, discover2) = new_discover();
        let connector = Arc::new(MockConnector::new());
        let picker: Arc<dyn ChannelPicker<ReadyChannel<MockService>, &'static str> + Send + Sync> =
            Arc::new(P2cPicker);
        let registry = OutlierStatsRegistry::with_rng(fp_config(50, 5, 3), Box::new(AlwaysFireRng));

        // First wiring succeeds.
        LoadBalancer::with_outlier(
            discover1,
            connector.clone(),
            picker.clone(),
            Some(registry.clone()),
        )
        .expect("first wire");

        // Second wiring of the same registry must error, not panic.
        let result =
            LoadBalancer::with_outlier(discover2, connector, picker, Some(registry.clone()));
        assert!(result.is_err());
    }
}
