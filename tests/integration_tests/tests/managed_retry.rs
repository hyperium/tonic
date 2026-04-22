//! End-to-end integration tests for `ManagedRetryLayer` / `ManagedRetryService`.
//!
//! Each test sets the global retry hooks, applies `ManagedRetryLayer` to a
//! real channel, then makes RPC calls through it.  A static mutex serialises
//! all tests in this file so that global-state mutations (hooks, throttler
//! factories) don't race.

use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::net::TcpListener;
use tonic::{
    datadog::rpcteam::{
        admin_only_reset_hooks, admin_only_set_custom_retry_hook,
        admin_only_set_custom_retry_throttler, RetryDecision, RetryPolicy, RetryThrottler,
    },
    transport::{ManagedRetryLayer, Server},
    Request, Response, Status,
};
use tower::ServiceBuilder;

// Serialise every test so that global hook/throttler state doesn't bleed
// between them when the test binary runs tests in parallel.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global test lock, recovering from a poisoned state so that one
/// panicking test doesn't prevent all subsequent tests from running.
fn acquire_test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ── Server helpers ────────────────────────────────────────────────────────────

/// Test service that records every call and returns a user-supplied error for
/// the first `fail_for` calls, then succeeds.
struct CountingSvc {
    calls: Arc<AtomicU32>,
    fail_for: u32,
    fail_with: Status,
}

#[tonic::async_trait]
impl test_server::Test for CountingSvc {
    async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= self.fail_for {
            Err(self.fail_with.clone())
        } else {
            Ok(Response::new(Output {}))
        }
    }
}

/// Spawns a test gRPC server and returns its address and a call-count handle.
async fn start_server(fail_for: u32, fail_with: Status) -> (SocketAddr, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    let svc = test_server::TestServer::new(CountingSvc {
        calls: calls.clone(),
        fail_for,
        fail_with,
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    // Give the server a moment to start.
    tokio::time::sleep(Duration::from_millis(10)).await;
    (addr, calls)
}

/// Builds a `TestClient` with `ManagedRetryLayer` applied to the channel.
///
/// Returns the concrete type so the compiler can verify the bounds required
/// by the generated client code.
fn client_with_retry(
    addr: SocketAddr,
) -> TestClient<tonic::transport::ManagedRetryService<tonic::transport::Channel>> {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_lazy();

    let svc = ServiceBuilder::new()
        .layer(ManagedRetryLayer)
        .service(channel);

    TestClient::new(svc)
}

fn valid_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
        backoff_multiplier: 1.5,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Without a hook registered the layer is transparent: no retries happen.
#[tokio::test]
async fn no_hook_no_retry() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    let (addr, calls) = start_server(1, Status::unavailable("transient")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_err(), "expected error when server fails and no retry hook");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "only one attempt without a hook");
}

/// A hook that always returns `NoRetry` should cause the call to fail after the
/// first attempt even if the server would succeed on a second try.
#[tokio::test]
async fn hook_no_retry_aborts_immediately() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();
    admin_only_set_custom_retry_hook(|_| RetryDecision::NoRetry, valid_retry_policy()).unwrap();

    let (addr, calls) = start_server(1, Status::unavailable("transient")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A hook that always returns `Undecided` should behave exactly like having no
/// hook at all — no retries.
#[tokio::test]
async fn hook_undecided_means_no_retry() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();
    admin_only_set_custom_retry_hook(|_| RetryDecision::Undecided, valid_retry_policy()).unwrap();

    let (addr, calls) = start_server(1, Status::unavailable("transient")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A hook that always returns `Retry` should transparently retry until the
/// server succeeds.
#[tokio::test]
async fn hook_retry_retries_until_success() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();
    admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, valid_retry_policy()).unwrap();

    // Server fails twice; the third call succeeds.
    let (addr, calls) = start_server(2, Status::unavailable("transient")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_ok(), "expected success after retries; got {:?}", result);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "initial + 2 retries expected");
}

/// A successful first call should not trigger any retry callback at all.
#[tokio::test]
async fn successful_call_makes_exactly_one_attempt() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    let attempt_count = Arc::new(AtomicU32::new(0));
    let attempt_count_clone = attempt_count.clone();

    struct CountingThrottler(Arc<AtomicU32>);
    impl RetryThrottler for CountingThrottler {
        fn throttle(&self) -> bool { false }
        fn attempt_started(&self, _is_retry: bool) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn attempt_completed(&self) {}
    }

    admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, valid_retry_policy()).unwrap();
    admin_only_set_custom_retry_throttler(move || {
        Box::new(CountingThrottler(attempt_count_clone.clone()))
    })
    .unwrap();

    // Server never fails.
    let (addr, _calls) = start_server(0, Status::ok("")).await;
    let mut client = client_with_retry(addr);

    client.unary_call(Request::new(Input {})).await.unwrap();
    assert_eq!(attempt_count.load(Ordering::SeqCst), 1);
}

/// When `max_attempts` is exhausted the final error is returned and the call
/// count on the server equals `max_attempts`.
#[tokio::test]
async fn max_attempts_exhausted_returns_error() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    let policy = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        backoff_multiplier: 1.0,
    };
    admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, policy).unwrap();

    // Always fails.
    let (addr, calls) = start_server(100, Status::internal("crash")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// A throttler that always returns `true` from `throttle()` should prevent any
/// retry even when the hook says `Retry`.
#[tokio::test]
async fn throttler_suppresses_retry() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, valid_retry_policy()).unwrap();

    struct AlwaysThrottle;
    impl RetryThrottler for AlwaysThrottle {
        fn throttle(&self) -> bool { true }
        fn attempt_started(&self, _: bool) {}
        fn attempt_completed(&self) {}
    }
    admin_only_set_custom_retry_throttler(|| Box::new(AlwaysThrottle)).unwrap();

    let (addr, calls) = start_server(5, Status::unavailable("busy")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_err());
    // Throttler fires after the first failure, so exactly 1 attempt reaches the server.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The throttler's `attempt_started` and `attempt_completed` callbacks must be
/// invoked once per attempt, with `is_retry = false` only for the first.
#[tokio::test]
async fn throttler_attempt_lifecycle_callbacks() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    admin_only_set_custom_retry_hook(
        |_| RetryDecision::Retry,
        RetryPolicy {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            backoff_multiplier: 1.0,
        },
    )
    .unwrap();

    let starts: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let completions = Arc::new(AtomicU32::new(0));

    {
        let starts_c = starts.clone();
        let completions_c = completions.clone();

        struct TrackThrottler {
            starts: Arc<Mutex<Vec<bool>>>,
            completions: Arc<AtomicU32>,
        }
        impl RetryThrottler for TrackThrottler {
            fn throttle(&self) -> bool { false }
            fn attempt_started(&self, is_retry: bool) {
                self.starts.lock().unwrap().push(is_retry);
            }
            fn attempt_completed(&self) {
                self.completions.fetch_add(1, Ordering::SeqCst);
            }
        }

        admin_only_set_custom_retry_throttler(move || {
            Box::new(TrackThrottler {
                starts: starts_c.clone(),
                completions: completions_c.clone(),
            })
        })
        .unwrap();
    }

    // Fails twice → 3 total attempts.
    let (addr, _calls) = start_server(2, Status::unavailable("x")).await;
    let mut client = client_with_retry(addr);

    client.unary_call(Request::new(Input {})).await.unwrap();

    let logged_starts = starts.lock().unwrap().clone();
    assert_eq!(logged_starts, vec![false, true, true], "is_retry flags incorrect");
    assert_eq!(completions.load(Ordering::SeqCst), 3, "completion count mismatch");
}

/// The hook receives the actual [`Status`] returned by the server, allowing it
/// to make decisions based on the gRPC status code.
#[tokio::test]
async fn hook_sees_correct_status_code() {
    let _g = acquire_test_lock();
    admin_only_reset_hooks();

    let seen_codes: Arc<Mutex<Vec<tonic::Code>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_codes_c = seen_codes.clone();

    // Hook retries on `Unavailable`, stops on anything else.
    admin_only_set_custom_retry_hook(
        move |status| {
            seen_codes_c.lock().unwrap().push(status.code());
            if status.code() == tonic::Code::Unavailable {
                RetryDecision::Retry
            } else {
                RetryDecision::NoRetry
            }
        },
        valid_retry_policy(),
    )
    .unwrap();

    // First call returns Unavailable, second succeeds.
    let (addr, calls) = start_server(1, Status::unavailable("transient")).await;
    let mut client = client_with_retry(addr);

    let result = client.unary_call(Request::new(Input {})).await;
    assert!(result.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let codes = seen_codes.lock().unwrap().clone();
    assert_eq!(codes, vec![tonic::Code::Unavailable]);
}
