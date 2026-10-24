/*
 *
 * Copyright 2026 gRPC authors.
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

//! [gRFC A32] circuit breaking for xDS clusters.
//!
//! Every endpoint attempt reserves a slot from an in-flight counter shared
//! process-wide by `{cluster, EDS service name}` and sized by the cluster's
//! CDS `max_requests`. When no slot is free the attempt fails locally with
//! `UNAVAILABLE` without reaching the endpoint; such drops are neither retried
//! nor counted by outlier detection. The slot is held until the response body
//! finishes, errors, or is dropped.
//!
//! [`CircuitBreakingMakeConnector`] applies this to gRPC channels at the
//! per-cluster connector, so both load-balancer backends enforce it.
//!
//! [gRFC A32]: https://github.com/grpc/proposal/blob/master/A32-xds-circuit-breaking.md

use std::fmt;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use dashmap::DashMap;
use http::{Request, Response};
use http_body::{Body, Frame};
use pin_project_lite::pin_project;
use tonic::body::Body as TonicBody;
use tower::load::Load;
use tower::{BoxError, Service};

use crate::client::endpoint::{ClusterConfig, Connector, EndpointAddress, MakeConnector};
use crate::common::async_util::BoxFuture;
use crate::xds::resource::circuit_breaking::CircuitBreakingConfig;

static GLOBAL_COUNTERS: OnceLock<Arc<ClusterRequestCounterState>> = OnceLock::new();

/// Shared circuit-breaking state for xDS clusters.
#[derive(Clone, Debug)]
pub(crate) struct ClusterCircuitBreakerRegistry {
    inner: Arc<ClusterCircuitBreakerRegistryInner>,
}

#[derive(Debug)]
struct ClusterCircuitBreakerRegistryInner {
    configs: DashMap<String, Arc<ClusterCircuitBreakerState>>,
    counters: ClusterRequestCounters,
}

impl Drop for ClusterCircuitBreakerRegistryInner {
    fn drop(&mut self) {
        for state in self.configs.iter() {
            if let Some(previous) = state.config.swap(None) {
                let counter_key = previous.counter_key.clone();
                previous.counter.deactivate();
                drop(previous);
                self.counters.cleanup_if_unused(&counter_key);
            }
        }
    }
}

impl ClusterCircuitBreakerRegistry {
    pub(crate) fn new() -> Self {
        Self::with_counters(ClusterRequestCounters::global())
    }

    fn with_counters(counters: ClusterRequestCounters) -> Self {
        Self {
            inner: Arc::new(ClusterCircuitBreakerRegistryInner {
                configs: DashMap::new(),
                counters,
            }),
        }
    }

    pub(crate) fn set_cluster_config(
        &self,
        cluster: impl Into<String>,
        eds_service_name: impl Into<String>,
        config: CircuitBreakingConfig,
    ) {
        let cluster = cluster.into();
        let eds_service_name = eds_service_name.into();
        let counter_key = CounterKey::new(cluster.as_str(), eds_service_name.as_str());
        let counter = self.inner.counters.counter(&counter_key);
        let state = self.ensure_state(&cluster);
        self.update_state_config(
            &state,
            CircuitBreakerRuntimeConfig {
                max_requests: config.max_requests,
                counter_key,
                counter,
            },
        );
    }

    fn ensure_state(&self, cluster: &str) -> Arc<ClusterCircuitBreakerState> {
        if let Some(state) = self.inner.configs.get(cluster) {
            return state.clone();
        }

        self.inner
            .configs
            .entry(cluster.to_string())
            .or_insert_with(|| Arc::new(ClusterCircuitBreakerState::new()))
            .clone()
    }

    fn cluster_breaker(&self, cluster: &str) -> Arc<ClusterCircuitBreaker> {
        let state = self.ensure_state(cluster);
        Arc::new(ClusterCircuitBreaker {
            cluster: Arc::from(cluster),
            state,
            counters: self.inner.counters.clone(),
        })
    }

    fn update_state_config(
        &self,
        state: &ClusterCircuitBreakerState,
        config: CircuitBreakerRuntimeConfig,
    ) {
        let _update_guard = state
            .update_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = state.current_config();
        if previous.as_deref() == Some(&config) {
            return;
        }

        let counter_key_changed = previous
            .as_ref()
            .is_none_or(|previous| previous.counter_key != config.counter_key);
        if counter_key_changed {
            config.counter.activate();
        }

        drop(previous);
        let previous = state.config.swap(Some(Arc::new(config)));
        if counter_key_changed && let Some(previous) = previous {
            self.deactivate_config(previous);
        }
    }

    #[cfg(test)]
    fn clear_state(&self, state: &ClusterCircuitBreakerState) {
        let _update_guard = state
            .update_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = state.config.swap(None) {
            self.deactivate_config(previous);
        }
    }

    fn deactivate_config(&self, config: Arc<CircuitBreakerRuntimeConfig>) {
        let counter_key = config.counter_key.clone();
        config.counter.deactivate();
        drop(config);
        self.inner.counters.cleanup_if_unused(&counter_key);
    }

    #[cfg(test)]
    fn acquire(&self, cluster: &str) -> Result<Option<CircuitBreakerPermit>, CircuitBreakerLimit> {
        self.cluster_breaker(cluster).acquire()
    }

    #[cfg(test)]
    fn in_flight(&self, cluster: &str) -> u32 {
        let breaker = self.cluster_breaker(cluster);
        breaker
            .state
            .current_config()
            .map(|config| self.inner.counters.in_flight(&config.counter_key))
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn dropped_requests(&self, cluster: &str) -> u64 {
        self.ensure_state(cluster).dropped_requests()
    }

    #[cfg(test)]
    fn counter_count(&self) -> usize {
        self.inner.counters.counter_count()
    }

    #[cfg(test)]
    fn clear_cluster_config(&self, cluster: &str) {
        let state = self.inner.configs.get(cluster).map(|state| state.clone());
        if let Some(state) = state {
            self.clear_state(&state);
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::with_counters(ClusterRequestCounters::isolated())
    }
}

impl Default for ClusterCircuitBreakerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
struct CircuitBreakerLimit {
    max_requests: u32,
}

#[derive(Clone, Debug)]
struct CircuitBreakerRuntimeConfig {
    max_requests: u32,
    counter_key: CounterKey,
    counter: Arc<InFlightCounter>,
}

impl PartialEq for CircuitBreakerRuntimeConfig {
    fn eq(&self, other: &Self) -> bool {
        self.max_requests == other.max_requests && self.counter_key == other.counter_key
    }
}

impl Eq for CircuitBreakerRuntimeConfig {}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CounterKey {
    cluster: Arc<str>,
    eds_service_name: Arc<str>,
}

impl CounterKey {
    fn new(cluster: impl Into<Arc<str>>, eds_service_name: impl Into<Arc<str>>) -> Self {
        Self {
            cluster: cluster.into(),
            eds_service_name: eds_service_name.into(),
        }
    }
}

struct ClusterCircuitBreakerState {
    config: ArcSwapOption<CircuitBreakerRuntimeConfig>,
    update_lock: Mutex<()>,
    dropped_requests: AtomicU64,
}

impl fmt::Debug for ClusterCircuitBreakerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterCircuitBreakerState")
            .field("current_config", &self.current_config())
            .field(
                "dropped_requests",
                &self.dropped_requests.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl ClusterCircuitBreakerState {
    fn new() -> Self {
        Self {
            config: ArcSwapOption::empty(),
            update_lock: Mutex::new(()),
            dropped_requests: AtomicU64::new(0),
        }
    }

    fn current_config(&self) -> Option<Arc<CircuitBreakerRuntimeConfig>> {
        self.config.load_full()
    }

    fn record_drop(&self) {
        self.dropped_requests.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    fn dropped_requests(&self) -> u64 {
        self.dropped_requests.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct ClusterCircuitBreaker {
    cluster: Arc<str>,
    state: Arc<ClusterCircuitBreakerState>,
    counters: ClusterRequestCounters,
}

impl ClusterCircuitBreaker {
    fn acquire(&self) -> Result<Option<CircuitBreakerPermit>, CircuitBreakerLimit> {
        let Some(config) = self.state.current_config() else {
            return Ok(None);
        };

        self.acquire_with_config(
            config.counter_key.clone(),
            config.counter.clone(),
            config.max_requests,
        )
        .map(Some)
    }

    fn acquire_with_config(
        &self,
        counter_key: CounterKey,
        counter: Arc<InFlightCounter>,
        max_requests: u32,
    ) -> Result<CircuitBreakerPermit, CircuitBreakerLimit> {
        let limit = CircuitBreakerLimit { max_requests };
        match self.counters.acquire(counter_key, counter, max_requests) {
            Some(permit) => Ok(permit),
            None => {
                self.state.record_drop();
                Err(limit)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ClusterRequestCounters {
    inner: Arc<ClusterRequestCounterState>,
}

#[derive(Debug, Default)]
struct ClusterRequestCounterState {
    counters: DashMap<CounterKey, Arc<InFlightCounter>>,
}

impl ClusterRequestCounters {
    fn global() -> Self {
        Self {
            inner: GLOBAL_COUNTERS
                .get_or_init(|| Arc::new(ClusterRequestCounterState::default()))
                .clone(),
        }
    }

    #[cfg(test)]
    fn isolated() -> Self {
        Self {
            inner: Arc::new(ClusterRequestCounterState::default()),
        }
    }

    fn acquire(
        &self,
        counter_key: CounterKey,
        counter: Arc<InFlightCounter>,
        limit: u32,
    ) -> Option<CircuitBreakerPermit> {
        if counter.try_acquire(limit) {
            Some(CircuitBreakerPermit {
                counter: Some(counter),
                counter_key,
                counters: self.clone(),
            })
        } else {
            let should_cleanup = counter.is_unused();
            drop(counter);
            if should_cleanup {
                self.cleanup_if_unused(&counter_key);
            }
            None
        }
    }

    fn counter(&self, counter_key: &CounterKey) -> Arc<InFlightCounter> {
        self.inner
            .counters
            .entry(counter_key.clone())
            .or_insert_with(|| Arc::new(InFlightCounter::default()))
            .clone()
    }

    fn cleanup_if_unused(&self, counter_key: &CounterKey) {
        self.inner.counters.remove_if(counter_key, |_, counter| {
            counter.is_unused() && Arc::strong_count(counter) == 1
        });
    }

    #[cfg(test)]
    fn in_flight(&self, counter_key: &CounterKey) -> u32 {
        self.inner
            .counters
            .get(counter_key)
            .map(|counter| counter.in_flight())
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn counter_count(&self) -> usize {
        self.inner.counters.len()
    }
}

#[derive(Debug, Default)]
struct InFlightCounter {
    in_flight: AtomicU32,
    active_refs: AtomicUsize,
}

impl InFlightCounter {
    fn activate(&self) {
        self.active_refs.fetch_add(1, Ordering::AcqRel);
    }

    fn deactivate(&self) {
        // TODO: Switch to `try_update` and remove this allow once MSRV >= 1.95.
        #[allow(deprecated)]
        let result = self
            .active_refs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
        assert!(
            result.is_ok(),
            "attempted to deactivate an inactive circuit breaker counter"
        );
    }

    fn try_acquire(&self, limit: u32) -> bool {
        loop {
            let current = self.in_flight.load(Ordering::Acquire);
            if current >= limit {
                return false;
            }

            if self
                .in_flight
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Acquire)
    }

    fn release(&self) {
        let previous = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous > 0,
            "attempted to release an inactive circuit breaker permit"
        );
    }

    fn is_unused(&self) -> bool {
        self.in_flight() == 0 && self.active_refs.load(Ordering::Acquire) == 0
    }
}

#[derive(Debug)]
struct CircuitBreakerPermit {
    counter: Option<Arc<InFlightCounter>>,
    counter_key: CounterKey,
    counters: ClusterRequestCounters,
}

impl Drop for CircuitBreakerPermit {
    fn drop(&mut self) {
        if let Some(counter) = self.counter.take() {
            counter.release();
            let should_cleanup = counter.is_unused();
            drop(counter);
            if should_cleanup {
                self.counters.cleanup_if_unused(&self.counter_key);
            }
        }
    }
}

/// A [`MakeConnector`] whose endpoints enforce their cluster's A32 limit.
///
/// Every connector built for a cluster wraps its endpoints in a
/// [`CircuitBreakingEndpointService`] bound to the cluster's shared breaker.
/// An accepted CDS update refreshes the limit before its connector is
/// published, so endpoints connected under an earlier update pick up the new
/// limit too; a rejected update changes neither.
///
/// gRPC-only: the permit is held by the [`TonicBody`] response body, which a
/// transport-generic response type cannot carry.
pub(crate) struct CircuitBreakingMakeConnector<MC> {
    inner: MC,
    circuit_breakers: ClusterCircuitBreakerRegistry,
}

impl<MC> CircuitBreakingMakeConnector<MC> {
    pub(crate) fn new(inner: MC, circuit_breakers: ClusterCircuitBreakerRegistry) -> Self {
        Self {
            inner,
            circuit_breakers,
        }
    }
}

impl<MC: MakeConnector> MakeConnector for CircuitBreakingMakeConnector<MC> {
    type Service = CircuitBreakingEndpointService<MC::Service>;

    fn make_connector(
        &self,
        cluster: ClusterConfig<'_>,
    ) -> Result<Arc<dyn Connector<Service = Self::Service> + Send + Sync>, BoxError> {
        let cluster_name = cluster.name().to_owned();
        let eds_service_name = cluster.eds_service_name;
        let config = cluster.circuit_breaking;
        let inner = self.inner.make_connector(cluster)?;
        self.circuit_breakers
            .set_cluster_config(cluster_name.as_str(), eds_service_name, config);
        Ok(Arc::new(CircuitBreakingConnector {
            inner,
            breaker: self.circuit_breakers.cluster_breaker(&cluster_name),
        }))
    }
}

struct CircuitBreakingConnector<S> {
    inner: Arc<dyn Connector<Service = S> + Send + Sync>,
    breaker: Arc<ClusterCircuitBreaker>,
}

impl<S: Send + 'static> Connector for CircuitBreakingConnector<S> {
    type Service = CircuitBreakingEndpointService<S>;

    fn connect(&self, addr: &EndpointAddress) -> BoxFuture<Self::Service> {
        let connecting = self.inner.connect(addr);
        let breaker = self.breaker.clone();
        Box::pin(async move { CircuitBreakingEndpointService::new(connecting.await, breaker) })
    }
}

/// An endpoint service that reserves a slot from its cluster's breaker for
/// every call.
///
/// The slot is taken when the call starts, after the load balancer has picked
/// this endpoint and found it ready, so it covers one endpoint attempt rather
/// than time spent queued or backed off between retries.
#[derive(Clone)]
pub(crate) struct CircuitBreakingEndpointService<S> {
    inner: S,
    breaker: Arc<ClusterCircuitBreaker>,
}

impl<S> CircuitBreakingEndpointService<S> {
    fn new(inner: S, breaker: Arc<ClusterCircuitBreaker>) -> Self {
        Self { inner, breaker }
    }
}

impl<S: fmt::Debug> fmt::Debug for CircuitBreakingEndpointService<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreakingEndpointService")
            .field("inner", &self.inner)
            .field("breaker", &self.breaker)
            .finish()
    }
}

impl<S, B> Service<Request<B>> for CircuitBreakingEndpointService<S>
where
    S: Service<Request<B>, Response = Response<TonicBody>, Error: Into<BoxError>>,
    S::Future: Send + 'static,
{
    type Response = Response<TonicBody>;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let permit = match self.breaker.acquire() {
            Ok(permit) => permit,
            Err(limit) => {
                return Box::pin(std::future::ready(Ok(limit_exceeded_response(
                    &self.breaker.cluster,
                    limit,
                ))));
            }
        };
        let response = self.inner.call(request);
        Box::pin(async move {
            let response = response.await.map_err(Into::into)?;
            match permit {
                Some(permit) => {
                    Ok(response.map(|body| TonicBody::new(PermitBody::new(body, permit))))
                }
                None => Ok(response),
            }
        })
    }
}

impl<S: Load> Load for CircuitBreakingEndpointService<S> {
    type Metric = S::Metric;

    fn load(&self) -> Self::Metric {
        self.inner.load()
    }
}

/// Marks responses rejected by the local circuit breaker before reaching an endpoint.
///
/// The retry layer uses this extension to distinguish local `UNAVAILABLE` drops
/// from retryable responses returned by an upstream service.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalCircuitBreakerDrop;

pub(crate) fn is_local_circuit_breaker_drop<B>(response: &Response<B>) -> bool {
    response
        .extensions()
        .get::<LocalCircuitBreakerDrop>()
        .is_some()
}

fn limit_exceeded_response(cluster: &str, limit: CircuitBreakerLimit) -> Response<TonicBody> {
    let mut response = status_response(tonic::Status::unavailable(format!(
        "circuit breaker open for cluster '{cluster}': max_requests limit {} reached",
        limit.max_requests,
    )));
    response.extensions_mut().insert(LocalCircuitBreakerDrop);
    response
}

fn status_response(status: tonic::Status) -> Response<TonicBody> {
    status.into_http::<TonicBody>()
}

pin_project! {
    #[derive(Debug)]
    struct PermitBody<B> {
        #[pin]
        inner: B,
        permit: Option<CircuitBreakerPermit>,
    }
}

impl<B> PermitBody<B> {
    fn new(inner: B, permit: CircuitBreakerPermit) -> Self {
        Self {
            inner,
            permit: Some(permit),
        }
    }
}

impl<B> Body for PermitBody<B>
where
    B: Body<Data = Bytes>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(None) => {
                this.permit.take();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(frame))) => {
                if frame.is_trailers() {
                    this.permit.take();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.permit.take();
                Poll::Ready(Some(Err(err)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use bytes::Bytes;
    use http::{HeaderMap, Request, Response};
    use http_body::{Body, Frame};
    use tonic::Code;
    use tower::ServiceExt;
    use tower::service_fn;

    use crate::client::retry::{
        GrpcRetryClassifier, RetryBackoffConfig, RetryConfig, RetryLayer, RetryPolicy,
    };

    use super::*;

    const CLUSTER: &str = "cluster-a";
    const EDS_SERVICE_NAME: &str = "eds-service-a";

    fn request() -> Request<TonicBody> {
        Request::new(TonicBody::empty())
    }

    fn endpoint<S>(
        breakers: &ClusterCircuitBreakerRegistry,
        service: S,
    ) -> CircuitBreakingEndpointService<S> {
        CircuitBreakingEndpointService::new(service, breakers.cluster_breaker(CLUSTER))
    }

    fn configured_breakers(max_requests: u32) -> ClusterCircuitBreakerRegistry {
        let breakers = ClusterCircuitBreakerRegistry::new_for_test();
        breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests },
        );
        breakers
    }

    #[tokio::test]
    async fn rejects_requests_when_cluster_limit_is_reached() {
        let breakers = configured_breakers(1);
        let calls = Arc::new(AtomicU32::new(0));
        let call_counter = calls.clone();
        let service = service_fn(move |_request: Request<TonicBody>| {
            call_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, BoxError>(Response::new(TonicBody::new(PendingBody))) }
        });
        let mut service = endpoint(&breakers, service);

        let first = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        let second = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        let status = tonic::Status::from_header_map(second.headers()).unwrap();
        assert_eq!(status.code(), Code::Unavailable);
        assert!(status.message().contains("max_requests limit 1"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(breakers.dropped_requests(CLUSTER), 1);

        drop(first);
        assert_eq!(breakers.in_flight(CLUSTER), 0);

        let _third = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cached_service_applies_live_limit_updates_without_resetting_in_flight() {
        let breakers = configured_breakers(2);
        let calls = Arc::new(AtomicU32::new(0));
        let call_counter = calls.clone();
        let service = service_fn(move |_request: Request<TonicBody>| {
            call_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, BoxError>(Response::new(TonicBody::new(PendingBody))) }
        });
        let mut service = endpoint(&breakers, service);

        let first = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        let second = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 2);

        breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests: 1 },
        );

        let third = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(third.headers())
                .unwrap()
                .code(),
            Code::Unavailable
        );

        drop(first);
        assert_eq!(breakers.in_flight(CLUSTER), 1);
        let fourth = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(fourth.headers())
                .unwrap()
                .code(),
            Code::Unavailable
        );

        drop(second);
        assert_eq!(breakers.in_flight(CLUSTER), 0);
        let _fifth = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(breakers.dropped_requests(CLUSTER), 2);
    }

    #[tokio::test]
    async fn releases_permit_when_response_body_reaches_trailers() {
        let breakers = configured_breakers(1);
        let service = service_fn(|_request: Request<TonicBody>| async {
            Ok::<_, BoxError>(Response::new(TonicBody::new(DataThenTrailersBody {
                state: BodyState::Data,
            })))
        });
        let mut service = endpoint(&breakers, service);

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        let mut body = response.into_body();
        let data_frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        assert!(data_frame.unwrap().unwrap().is_data());
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        let trailers_frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        assert!(trailers_frame.unwrap().unwrap().is_trailers());
        assert_eq!(breakers.in_flight(CLUSTER), 0);
    }

    #[tokio::test]
    async fn releases_permit_when_response_body_returns_error() {
        let breakers = configured_breakers(1);
        let service = service_fn(|_request: Request<TonicBody>| async {
            Ok::<_, BoxError>(Response::new(TonicBody::new(ErrorBody { emitted: false })))
        });
        let mut service = endpoint(&breakers, service);

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        let mut body = response.into_body();
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        assert!(frame.unwrap().is_err());
        assert_eq!(breakers.in_flight(CLUSTER), 0);
    }

    #[tokio::test]
    async fn releases_permit_when_response_future_is_dropped() {
        let breakers = configured_breakers(1);
        let service = service_fn(|_request: Request<TonicBody>| async {
            std::future::pending::<Result<Response<TonicBody>, BoxError>>().await
        });
        let mut service = endpoint(&breakers, service);

        let mut future = service.ready().await.unwrap().call(request());
        std::future::poll_fn(|cx| match Future::poll(Pin::new(&mut future), cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("inner service should remain pending"),
        })
        .await;
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        drop(future);
        assert_eq!(breakers.in_flight(CLUSTER), 0);
    }

    #[tokio::test]
    async fn oneshot_honors_config_after_consuming_service() {
        let calls = Arc::new(AtomicU32::new(0));
        let call_counter = calls.clone();
        let service = service_fn(move |_request: Request<TonicBody>| {
            call_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, BoxError>(Response::new(TonicBody::empty())) }
        });
        let breakers = configured_breakers(0);
        let service = endpoint(&breakers, service);

        let response = service.oneshot(request()).await.unwrap();
        let status = tonic::Status::from_header_map(response.headers()).unwrap();

        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn limit_responses_do_not_enter_retry_policy() {
        let breakers = configured_breakers(0);
        let policy = RetryPolicy::new(
            RetryConfig::new().num_retries(4).retry_backoff(
                RetryBackoffConfig::new(Duration::from_millis(1))
                    .max_interval(Duration::from_millis(1)),
            ),
            Arc::new(GrpcRetryClassifier::new(vec![Code::Unavailable])),
        );
        let calls = Arc::new(AtomicU32::new(0));
        let call_counter = calls.clone();

        let service = service_fn(
            move |_request: Request<shared_http_body::SharedBody<TonicBody>>| {
                call_counter.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, BoxError>(Response::new(TonicBody::empty())) }
            },
        );
        let mut service = tower::ServiceBuilder::new()
            .layer(RetryLayer::new(policy.into_shared()))
            .service(endpoint(&breakers, service));

        let response = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        let status = tonic::Status::from_header_map(response.headers()).unwrap();

        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(breakers.dropped_requests(CLUSTER), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn shared_counters_enforce_process_limit_with_per_channel_drop_counts() {
        let counters = ClusterRequestCounters::isolated();
        let first_breakers = ClusterCircuitBreakerRegistry::with_counters(counters.clone());
        let second_breakers = ClusterCircuitBreakerRegistry::with_counters(counters.clone());
        first_breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests: 1 },
        );
        second_breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests: 1 },
        );

        let first = first_breakers.acquire(CLUSTER).unwrap().unwrap();
        assert!(second_breakers.acquire(CLUSTER).is_err());
        assert_eq!(first_breakers.dropped_requests(CLUSTER), 0);
        assert_eq!(second_breakers.dropped_requests(CLUSTER), 1);

        drop(first);
        let second = second_breakers.acquire(CLUSTER).unwrap().unwrap();
        drop(second);
        drop(first_breakers);
        drop(second_breakers);
        assert_eq!(counters.counter_count(), 0);
    }

    #[test]
    fn unconfigured_cluster_has_no_limit_or_counter() {
        let breakers = ClusterCircuitBreakerRegistry::new_for_test();
        let breaker = breakers.cluster_breaker(CLUSTER);

        for _ in 0..2048 {
            assert!(breaker.acquire().unwrap().is_none());
        }

        assert_eq!(breakers.dropped_requests(CLUSTER), 0);
        assert_eq!(breakers.counter_count(), 0);
    }

    #[test]
    fn eds_service_name_change_uses_independent_counter() {
        let breakers = ClusterCircuitBreakerRegistry::new_for_test();
        breakers.set_cluster_config(CLUSTER, "eds-a", CircuitBreakingConfig { max_requests: 1 });
        let first = breakers.acquire(CLUSTER).unwrap().unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 1);

        breakers.set_cluster_config(CLUSTER, "eds-b", CircuitBreakingConfig { max_requests: 1 });
        assert_eq!(breakers.in_flight(CLUSTER), 0);
        let second = breakers.acquire(CLUSTER).unwrap().unwrap();
        assert_eq!(breakers.in_flight(CLUSTER), 1);
        assert!(breakers.acquire(CLUSTER).is_err());

        drop(second);
        assert_eq!(breakers.in_flight(CLUSTER), 0);
        drop(first);
        assert_eq!(breakers.counter_count(), 1);
    }

    #[test]
    fn idle_eds_service_name_change_cleans_up_previous_counter() {
        let counters = ClusterRequestCounters::isolated();
        let breakers = ClusterCircuitBreakerRegistry::with_counters(counters.clone());
        breakers.set_cluster_config(CLUSTER, "eds-a", CircuitBreakingConfig { max_requests: 1 });
        assert_eq!(counters.counter_count(), 1);

        breakers.set_cluster_config(CLUSTER, "eds-b", CircuitBreakingConfig { max_requests: 1 });
        assert_eq!(counters.counter_count(), 1);

        drop(breakers);
        assert_eq!(counters.counter_count(), 0);
    }

    #[test]
    fn cluster_removal_cleans_up_counter_after_in_flight_requests_finish() {
        let breakers = configured_breakers(1);
        let permit = breakers.acquire(CLUSTER).unwrap().unwrap();
        assert_eq!(breakers.counter_count(), 1);

        breakers.clear_cluster_config(CLUSTER);
        assert_eq!(breakers.counter_count(), 1);

        drop(permit);
        assert_eq!(breakers.counter_count(), 0);
    }

    #[tokio::test]
    async fn cached_breaker_observes_cluster_removal_and_recreation() {
        let breakers = configured_breakers(1);
        let calls = Arc::new(AtomicU32::new(0));
        let call_counter = calls.clone();
        let service = service_fn(move |_request: Request<TonicBody>| {
            call_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, BoxError>(Response::new(TonicBody::empty())) }
        });
        let mut service = endpoint(&breakers, service);

        let first = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        drop(first);

        breakers.clear_cluster_config(CLUSTER);
        let second = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        assert!(tonic::Status::from_header_map(second.headers()).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests: 0 },
        );
        let third = service
            .ready()
            .await
            .unwrap()
            .call(request())
            .await
            .unwrap();
        let status = tonic::Status::from_header_map(third.headers()).unwrap();
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn dropping_breakers_releases_config_counter_ref() {
        let counters = ClusterRequestCounters::isolated();
        let breakers = ClusterCircuitBreakerRegistry::with_counters(counters.clone());
        breakers.set_cluster_config(
            CLUSTER,
            EDS_SERVICE_NAME,
            CircuitBreakingConfig { max_requests: 1 },
        );
        let permit = breakers.acquire(CLUSTER).unwrap().unwrap();
        drop(permit);
        assert_eq!(counters.counter_count(), 1);

        drop(breakers);

        assert_eq!(counters.counter_count(), 0);
    }

    #[test]
    fn cleanup_keeps_counter_with_outstanding_clone() {
        let counters = ClusterRequestCounters::isolated();
        let counter_key = CounterKey::new(CLUSTER, EDS_SERVICE_NAME);
        let counter = counters.counter(&counter_key);

        counters.cleanup_if_unused(&counter_key);
        assert_eq!(counters.counter_count(), 1);

        drop(counter);
        counters.cleanup_if_unused(&counter_key);
        assert_eq!(counters.counter_count(), 0);
    }

    #[test]
    fn structured_counter_keys_do_not_collide_on_embedded_delimiters() {
        let counters = ClusterRequestCounters::isolated();
        let first_key = CounterKey::new("cluster\0eds", "service");
        let second_key = CounterKey::new("cluster", "eds\0service");

        let first = counters.counter(&first_key);
        let second = counters.counter(&second_key);

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(counters.counter_count(), 2);
    }

    #[tokio::test]
    async fn waiting_for_inner_readiness_does_not_acquire_permit() {
        let breakers = configured_breakers(1);
        let calls = Arc::new(AtomicU32::new(0));
        let service = BackpressuredService {
            ready_budget: Arc::new(AtomicU32::new(0)),
            calls: calls.clone(),
        };
        let mut service = endpoint(&breakers, service);

        let mut ready = Box::pin(service.ready());
        std::future::poll_fn(|cx| match ready.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("inner service should remain backpressured"),
        })
        .await;

        assert_eq!(breakers.in_flight(CLUSTER), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Every CDS update that yields a connector refreshes the cluster's limit,
    /// including for endpoints connected before it; a rejected one does not.
    #[tokio::test]
    async fn accepted_cds_updates_set_the_limit_for_connected_endpoints() {
        let make = CircuitBreakingMakeConnector::new(
            TestMakeConnector::default(),
            ClusterCircuitBreakerRegistry::new_for_test(),
        );
        let connector = make_connector(&make, 1).unwrap();
        let mut endpoint = connector
            .connect(&EndpointAddress::new("10.0.0.1", 8080))
            .await;
        let mut call = async || {
            endpoint
                .ready()
                .await
                .unwrap()
                .call(request())
                .await
                .unwrap()
        };

        let first = call().await;
        assert!(tonic::Status::from_header_map(first.headers()).is_none());
        assert!(is_local_circuit_breaker_drop(&call().await));

        make.inner.reject.store(true, Ordering::SeqCst);
        assert!(make_connector(&make, 2).is_err());
        assert!(is_local_circuit_breaker_drop(&call().await));

        make.inner.reject.store(false, Ordering::SeqCst);
        make_connector(&make, 2).unwrap();
        let second = call().await;
        assert!(tonic::Status::from_header_map(second.headers()).is_none());
    }

    fn make_connector(
        make: &CircuitBreakingMakeConnector<TestMakeConnector>,
        max_requests: u32,
    ) -> Result<
        Arc<dyn Connector<Service = CircuitBreakingEndpointService<PendingEndpoint>> + Send + Sync>,
        BoxError,
    > {
        use crate::xds::resource::cluster::{ClusterResource, LbPolicy};

        let cluster = ClusterResource {
            name: CLUSTER.to_string(),
            eds_service_name: None,
            lb_policy: LbPolicy::RoundRobin,
            security: None,
            circuit_breaking: CircuitBreakingConfig { max_requests },
        };
        #[cfg(feature = "_tls-any")]
        let registry = crate::xds::cert_provider::CertProviderRegistry::from_bootstrap(
            &Default::default(),
            Default::default(),
        )
        .unwrap();
        make.make_connector(ClusterConfig::from_resource(
            &cluster,
            #[cfg(feature = "_tls-any")]
            &registry,
        ))
    }

    /// Builds connectors to [`PendingEndpoint`]s, or rejects the update while
    /// `reject` is set.
    #[derive(Default)]
    struct TestMakeConnector {
        reject: std::sync::atomic::AtomicBool,
    }

    impl MakeConnector for TestMakeConnector {
        type Service = PendingEndpoint;

        fn make_connector(
            &self,
            _cluster: ClusterConfig<'_>,
        ) -> Result<Arc<dyn Connector<Service = PendingEndpoint> + Send + Sync>, BoxError> {
            if self.reject.load(Ordering::SeqCst) {
                return Err("rejected".into());
            }
            Ok(Arc::new(PendingConnector))
        }
    }

    struct PendingConnector;

    impl Connector for PendingConnector {
        type Service = PendingEndpoint;

        fn connect(&self, _addr: &EndpointAddress) -> BoxFuture<PendingEndpoint> {
            Box::pin(async { PendingEndpoint })
        }
    }

    /// An endpoint whose responses stay in flight until dropped.
    #[derive(Clone)]
    struct PendingEndpoint;

    impl Service<Request<TonicBody>> for PendingEndpoint {
        type Response = Response<TonicBody>;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: Request<TonicBody>) -> Self::Future {
            std::future::ready(Ok(Response::new(TonicBody::new(PendingBody))))
        }
    }

    #[derive(Clone, Debug)]
    struct BackpressuredService {
        ready_budget: Arc<AtomicU32>,
        calls: Arc<AtomicU32>,
    }

    impl Service<Request<TonicBody>> for BackpressuredService {
        type Response = Response<TonicBody>;
        type Error = BoxError;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        // TODO: Switch to `try_update` and remove this allow once MSRV >= 1.95.
        #[allow(deprecated)]
        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if self
                .ready_budget
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }

        fn call(&mut self, _request: Request<TonicBody>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(Response::new(TonicBody::new(PendingBody))))
        }
    }

    #[derive(Debug)]
    struct PendingBody;

    impl Body for PendingBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Pending
        }
    }

    #[derive(Debug)]
    struct ErrorBody {
        emitted: bool,
    }

    impl Body for ErrorBody {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if self.emitted {
                Poll::Ready(None)
            } else {
                self.emitted = true;
                Poll::Ready(Some(Err(tonic::Status::internal("body failed"))))
            }
        }
    }

    #[derive(Debug)]
    enum BodyState {
        Data,
        Trailers,
        Done,
    }

    #[derive(Debug)]
    struct DataThenTrailersBody {
        state: BodyState,
    }

    impl Body for DataThenTrailersBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            match self.state {
                BodyState::Data => {
                    self.state = BodyState::Trailers;
                    Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"hello")))))
                }
                BodyState::Trailers => {
                    self.state = BodyState::Done;
                    Poll::Ready(Some(Ok(Frame::trailers(HeaderMap::new()))))
                }
                BodyState::Done => Poll::Ready(None),
            }
        }
    }
}
