//! Circuit-breaker middleware for tonic services.
//!
//! Wraps any Tower [`Service`] and prevents calls to a struggling downstream
//! when too many consecutive failures have been observed, returning
//! [`Status::unavailable`] immediately until the service shows signs of
//! recovery.
//!
//! # State machine
//!
//! ```text
//!  ┌────────┐  consecutive_failures >= threshold  ┌──────┐
//!  │ Closed │ ─────────────────────────────────► │ Open │
//!  └────────┘                                     └──────┘
//!      ▲                                              │
//!      │  success_rate >= success_threshold           │ timeout elapsed
//!      │                                              ▼
//!      └────────────────────────────────── ┌──────────┐
//!                                          │ HalfOpen │
//!                                          └──────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use tonic::service::circuit_breaker::CircuitBreakerLayer;
//! use tower::ServiceBuilder;
//! use std::time::Duration;
//!
//! let channel = tonic::transport::Channel::from_static("http://[::1]:50051")
//!     .connect()
//!     .await?;
//!
//! let channel = ServiceBuilder::new()
//!     .layer(CircuitBreakerLayer::new(5, 0.6, Duration::from_secs(30)))
//!     .service(channel);
//!
//! let mut client = MyServiceClient::new(channel);
//! ```

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use pin_project::pin_project;
use tower_layer::Layer;
use tower_service::Service;

use crate::Status;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum CircuitState {
    /// Normal operation — requests flow through.
    Closed,
    /// Too many failures — requests are rejected with `Status::unavailable`.
    Open,
    /// One probe request allowed through to test recovery.
    HalfOpen,
}

#[derive(Debug)]
struct State {
    status: CircuitState,
    consecutive_failures: usize,
    last_failure: Option<Instant>,
    last_transition: Instant,
    /// Sliding window of outcomes: `true` = success, `false` = failure.
    window: Vec<bool>,
}

impl State {
    fn new() -> Self {
        Self {
            status: CircuitState::Closed,
            consecutive_failures: 0,
            last_failure: None,
            last_transition: Instant::now(),
            window: Vec::with_capacity(100),
        }
    }

    fn push(&mut self, success: bool) {
        self.window.push(success);
        if self.window.len() > 100 {
            self.window.remove(0);
        }
    }

    fn success_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        self.window.iter().filter(|&&v| v).count() as f64 / self.window.len() as f64
    }
}

// ── Layer ─────────────────────────────────────────────────────────────────────

/// [`Layer`] that applies [`CircuitBreaker`] middleware.
///
/// [`Layer`]: tower_layer::Layer
#[derive(Clone, Debug)]
pub struct CircuitBreakerLayer {
    failure_threshold: usize,
    success_threshold: f64,
    timeout: Duration,
}

impl CircuitBreakerLayer {
    /// Create a new [`CircuitBreakerLayer`].
    ///
    /// - `failure_threshold`: consecutive failures before opening the circuit.
    /// - `success_threshold`: fraction of successes in the sliding window required to close
    ///   the circuit from `HalfOpen` state (e.g. `0.6` means 60%).
    /// - `timeout`: how long to wait in `Open` state before allowing a single probe request.
    pub fn new(failure_threshold: usize, success_threshold: f64, timeout: Duration) -> Self {
        Self {
            failure_threshold,
            success_threshold,
            timeout,
        }
    }
}

impl<S> Layer<S> for CircuitBreakerLayer {
    type Service = CircuitBreaker<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CircuitBreaker::new(
            inner,
            self.failure_threshold,
            self.success_threshold,
            self.timeout,
        )
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

/// Circuit-breaker middleware for tonic services.
///
/// See the [module documentation](self) for a usage example.
#[derive(Clone)]
pub struct CircuitBreaker<S> {
    inner: S,
    state: Arc<Mutex<State>>,
    failure_threshold: usize,
    success_threshold: f64,
    timeout: Duration,
}

impl<S: fmt::Debug> fmt::Debug for CircuitBreaker<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("inner", &self.inner)
            .field("failure_threshold", &self.failure_threshold)
            .field("success_threshold", &self.success_threshold)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl<S> CircuitBreaker<S> {
    /// Wrap `inner` with circuit-breaker protection.
    pub fn new(
        inner: S,
        failure_threshold: usize,
        success_threshold: f64,
        timeout: Duration,
    ) -> Self {
        Self {
            inner,
            state: Arc::new(Mutex::new(State::new())),
            failure_threshold,
            success_threshold,
            timeout,
        }
    }
}

impl<S, Req> Service<Req> for CircuitBreaker<S>
where
    S: Service<Req> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<crate::BoxError> + Send + 'static,
    S::Response: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = crate::BoxError;
    type Future = CircuitBreakerFuture<S::Future, S::Response>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Gate: check circuit state before advertising readiness.
        {
            let mut s = self.state.lock().unwrap();
            match s.status {
                CircuitState::Open => {
                    let elapsed = s
                        .last_failure
                        .map(|t| t.elapsed())
                        .unwrap_or(Duration::ZERO);

                    if elapsed < self.timeout {
                        return Poll::Ready(Err(
                            Status::unavailable("circuit breaker is open").into()
                        ));
                    }

                    // Timeout elapsed — probe with a single request.
                    s.status = CircuitState::HalfOpen;
                    s.window.clear();
                    s.consecutive_failures = 0;
                    s.last_transition = Instant::now();
                }
                CircuitState::Closed | CircuitState::HalfOpen => {}
            }
        }

        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let state = self.state.clone();
        let failure_threshold = self.failure_threshold;
        let success_threshold = self.success_threshold;

        let mut inner = self.inner.clone();
        std::mem::swap(&mut inner, &mut self.inner);

        CircuitBreakerFuture {
            inner: inner.call(req),
            state,
            failure_threshold,
            success_threshold,
            _marker: std::marker::PhantomData,
        }
    }
}

// ── Future ────────────────────────────────────────────────────────────────────

/// Response future for [`CircuitBreaker`].
#[pin_project]
pub struct CircuitBreakerFuture<F, T> {
    #[pin]
    inner: F,
    state: Arc<Mutex<State>>,
    failure_threshold: usize,
    success_threshold: f64,
    _marker: std::marker::PhantomData<T>,
}

impl<F, T> fmt::Debug for CircuitBreakerFuture<F, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreakerFuture").finish()
    }
}

impl<F, T, E> Future for CircuitBreakerFuture<F, T>
where
    F: Future<Output = Result<T, E>>,
    E: Into<crate::BoxError>,
{
    type Output = Result<T, crate::BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let failure_threshold = *this.failure_threshold;
        let success_threshold = *this.success_threshold;

        match this.inner.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(resp)) => {
                let mut s = this.state.lock().unwrap();
                s.push(true);
                match s.status {
                    CircuitState::HalfOpen => {
                        if s.success_rate() >= success_threshold {
                            s.status = CircuitState::Closed;
                            s.consecutive_failures = 0;
                            s.last_transition = Instant::now();
                        }
                    }
                    CircuitState::Closed => {
                        s.consecutive_failures = 0;
                    }
                    CircuitState::Open => {}
                }
                Poll::Ready(Ok(resp))
            }
            Poll::Ready(Err(e)) => {
                let mut s = this.state.lock().unwrap();
                s.push(false);
                s.consecutive_failures += 1;
                s.last_failure = Some(Instant::now());
                match s.status {
                    CircuitState::Closed => {
                        if s.consecutive_failures >= failure_threshold {
                            s.status = CircuitState::Open;
                            s.last_transition = Instant::now();
                        }
                    }
                    CircuitState::HalfOpen => {
                        s.status = CircuitState::Open;
                        s.last_transition = Instant::now();
                    }
                    CircuitState::Open => {}
                }
                Poll::Ready(Err(e.into()))
            }
        }
    }
}
