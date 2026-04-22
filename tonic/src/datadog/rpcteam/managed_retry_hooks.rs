//! Custom retry hooks for Datadog's RPC team.
//!
//! This module exposes two pluggable extension points that let the RPC team
//! inject managed retry logic directly into the tonic transport layer:
//!
//! 1. A **retry hook** — a function that inspects each failed [`Status`] and
//!    returns a [`RetryDecision`].  When it returns [`RetryDecision::Retry`]
//!    the transport will retry the RPC using the registered [`RetryPolicy`].
//!
//! 2. A **retry throttler factory** — a function that constructs one
//!    [`RetryThrottler`] per connection.  The throttler can suppress retries
//!    when too many are already in flight or when the error budget is
//!    exhausted.
//!
//! Both hooks are set exactly once at process start via the `admin_only_*`
//! functions.  They are intentionally placed behind a discouraging naming
//! convention; **do not use them outside of the RPC team's managed-retry
//! library**.

use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use crate::Status;

// ── RetryThrottler ────────────────────────────────────────────────────────────

/// A pluggable retry-throttling policy.
///
/// The transport calls these methods around every RPC attempt (including the
/// initial, non-retry attempt) so that the implementation can maintain an
/// accurate budget across all in-flight work on the connection.
///
/// Implementations **must** be `Send + Sync` and must not block.
pub trait RetryThrottler: Send + Sync {
    /// Returns `true` when the next retry should be suppressed.
    ///
    /// Called after [`attempt_started`] returns and before the retry attempt
    /// is dispatched, but only when the hook has already decided to retry.
    fn throttle(&self) -> bool;

    /// Called at the start of every RPC attempt, including the first.
    ///
    /// `is_retry` is `false` for the initial attempt and `true` for every
    /// subsequent retry.
    fn attempt_started(&self, is_retry: bool);

    /// Called at the completion of every RPC attempt, regardless of outcome.
    fn attempt_completed(&self);
}

struct NoopThrottler;

impl RetryThrottler for NoopThrottler {
    fn throttle(&self) -> bool {
        false
    }
    fn attempt_started(&self, _is_retry: bool) {}
    fn attempt_completed(&self) {}
}

// ── RetryDecision ─────────────────────────────────────────────────────────────

/// The decision returned by the custom retry hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// The hook did not have enough information to decide.
    ///
    /// The transport will fall through to its default behaviour (no retry in
    /// tonic's current implementation).
    Undecided,
    /// The hook decided to retry using the registered [`RetryPolicy`].
    Retry,
    /// The hook decided that this RPC must not be retried.
    NoRetry,
}

// ── RetryPolicy ───────────────────────────────────────────────────────────────

/// Parameters that govern retry behaviour when the hook returns
/// [`RetryDecision::Retry`].
///
/// `max_attempts` counts every attempt including the initial one, so a value
/// of `3` means the original request plus up to two retries.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total number of attempts (≥ 2; includes the initial attempt).
    pub max_attempts: u32,
    /// Backoff applied before the first retry.
    pub initial_backoff: Duration,
    /// Upper bound on backoff duration.
    pub max_backoff: Duration,
    /// Multiplier applied to the backoff after each retry (> 0.0).
    pub backoff_multiplier: f64,
}

// ── Global state ──────────────────────────────────────────────────────────────

struct RetryHook {
    hook: Box<dyn Fn(&Status) -> RetryDecision + Send + Sync>,
    policy: RetryPolicy,
}

type ThrottlerFactory = Box<dyn Fn() -> Box<dyn RetryThrottler> + Send + Sync>;

fn retry_hook_state() -> &'static RwLock<Option<RetryHook>> {
    static CELL: OnceLock<RwLock<Option<RetryHook>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

fn throttler_factory_state() -> &'static RwLock<Option<ThrottlerFactory>> {
    static CELL: OnceLock<RwLock<Option<ThrottlerFactory>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Registers the custom retry hook and the policy used when it decides to
/// retry.
///
/// Returns an error if `policy` fails validation (see [`RetryPolicy`] field
/// docs for constraints).
///
/// # ⚠ ONLY INTENDED FOR RPC TEAM USAGE
///
/// Do not call this function outside of Datadog's managed-retry library.
pub fn admin_only_set_custom_retry_hook(
    hook: impl Fn(&Status) -> RetryDecision + Send + Sync + 'static,
    policy: RetryPolicy,
) -> Result<(), String> {
    validate_retry_policy(&policy)?;
    *retry_hook_state().write().unwrap() = Some(RetryHook {
        hook: Box::new(hook),
        policy,
    });
    Ok(())
}

/// Registers a factory that produces one [`RetryThrottler`] per connection.
///
/// The factory is called during connection setup; every connection gets its
/// own independent throttler instance.
///
/// # ⚠ ONLY INTENDED FOR RPC TEAM USAGE
///
/// Do not call this function outside of Datadog's managed-retry library.
pub fn admin_only_set_custom_retry_throttler(
    factory: impl Fn() -> Box<dyn RetryThrottler> + Send + Sync + 'static,
) -> Result<(), String> {
    *throttler_factory_state().write().unwrap() = Some(Box::new(factory));
    Ok(())
}

/// Called by the retry service to decide whether a failed RPC should be
/// retried.
///
/// Three possible outcomes:
///
/// | Return value      | Meaning                                          |
/// |-------------------|--------------------------------------------------|
/// | `Ok(None)`        | No hook registered, or hook returned `Undecided` |
/// | `Ok(Some(policy))`| Hook returned `Retry`; use the returned policy   |
/// | `Err(())`         | Hook returned `NoRetry`; abort immediately       |
pub fn try_custom_retry(status: &Status) -> Result<Option<RetryPolicy>, ()> {
    let guard = retry_hook_state().read().unwrap();
    match guard.as_ref() {
        None => Ok(None),
        Some(h) => match (h.hook)(status) {
            RetryDecision::Retry => Ok(Some(h.policy.clone())),
            RetryDecision::NoRetry => Err(()),
            RetryDecision::Undecided => Ok(None),
        },
    }
}

/// Creates a new [`RetryThrottler`] for a single connection.
///
/// If a factory was registered via
/// [`admin_only_set_custom_retry_throttler`], calls it to obtain an
/// instance. Otherwise returns a no-op implementation that never throttles.
pub fn new_retry_throttler() -> Box<dyn RetryThrottler> {
    let guard = throttler_factory_state().read().unwrap();
    match guard.as_ref() {
        Some(factory) => factory(),
        None => Box::new(NoopThrottler),
    }
}

// ── Validation ────────────────────────────────────────────────────────────────

fn validate_retry_policy(p: &RetryPolicy) -> Result<(), String> {
    if p.max_attempts <= 1 {
        return Err("max_attempts must be greater than 1".into());
    }
    if p.initial_backoff.is_zero() {
        return Err("initial_backoff must be greater than zero".into());
    }
    if p.max_backoff.is_zero() {
        return Err("max_backoff must be greater than zero".into());
    }
    if p.backoff_multiplier <= 0.0 {
        return Err("backoff_multiplier must be greater than zero".into());
    }
    Ok(())
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Clears all registered hooks and throttler factories, resetting global
/// state to its initial empty condition.
///
/// This is intended for use between tests.  **Do not call in production
/// code.**
pub fn admin_only_reset_hooks() {
    *retry_hook_state().write().unwrap() = None;
    *throttler_factory_state().write().unwrap() = None;
}

/// Alias for [`admin_only_reset_hooks`] used within this crate's own tests.
#[cfg(test)]
pub(crate) fn reset_for_testing() {
    admin_only_reset_hooks();
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // Serialise tests that touch global state so they don't interfere with
    // each other when the test binary runs tests in parallel.
    static GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn valid_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
            backoff_multiplier: 2.0,
        }
    }

    // ── validate_retry_policy ────────────────────────────────────────────────

    #[test]
    fn validate_rejects_max_attempts_one() {
        let mut p = valid_policy();
        p.max_attempts = 1;
        assert!(validate_retry_policy(&p).is_err());
    }

    #[test]
    fn validate_rejects_zero_initial_backoff() {
        let mut p = valid_policy();
        p.initial_backoff = Duration::ZERO;
        assert!(validate_retry_policy(&p).is_err());
    }

    #[test]
    fn validate_rejects_zero_max_backoff() {
        let mut p = valid_policy();
        p.max_backoff = Duration::ZERO;
        assert!(validate_retry_policy(&p).is_err());
    }

    #[test]
    fn validate_rejects_zero_multiplier() {
        let mut p = valid_policy();
        p.backoff_multiplier = 0.0;
        assert!(validate_retry_policy(&p).is_err());
    }

    #[test]
    fn validate_rejects_negative_multiplier() {
        let mut p = valid_policy();
        p.backoff_multiplier = -1.0;
        assert!(validate_retry_policy(&p).is_err());
    }

    #[test]
    fn validate_accepts_valid_policy() {
        assert!(validate_retry_policy(&valid_policy()).is_ok());
    }

    // ── try_custom_retry ──────────────────────────────────────────────────────

    #[test]
    fn try_custom_retry_returns_none_when_no_hook() {
        let _g = lock();
        reset_for_testing();

        let status = Status::internal("boom");
        assert!(matches!(try_custom_retry(&status), Ok(None)));
    }

    #[test]
    fn try_custom_retry_returns_policy_on_retry_decision() {
        let _g = lock();
        reset_for_testing();

        admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, valid_policy()).unwrap();

        let status = Status::unavailable("transient");
        let result = try_custom_retry(&status);
        assert!(matches!(result, Ok(Some(_))));
        let policy = result.unwrap().unwrap();
        assert_eq!(policy.max_attempts, 3);
    }

    #[test]
    fn try_custom_retry_returns_err_on_no_retry_decision() {
        let _g = lock();
        reset_for_testing();

        admin_only_set_custom_retry_hook(|_| RetryDecision::NoRetry, valid_policy()).unwrap();

        let status = Status::permission_denied("blocked");
        assert!(try_custom_retry(&status).is_err());
    }

    #[test]
    fn try_custom_retry_returns_none_on_undecided() {
        let _g = lock();
        reset_for_testing();

        admin_only_set_custom_retry_hook(|_| RetryDecision::Undecided, valid_policy()).unwrap();

        let status = Status::unknown("?");
        assert!(matches!(try_custom_retry(&status), Ok(None)));
    }

    #[test]
    fn hook_receives_correct_status() {
        let _g = lock();
        reset_for_testing();

        let seen_code = Arc::new(Mutex::new(None::<crate::Code>));
        let seen_code_clone = seen_code.clone();

        admin_only_set_custom_retry_hook(
            move |s| {
                *seen_code_clone.lock().unwrap() = Some(s.code());
                RetryDecision::NoRetry
            },
            valid_policy(),
        )
        .unwrap();

        let status = Status::resource_exhausted("quota");
        let _ = try_custom_retry(&status);
        assert_eq!(*seen_code.lock().unwrap(), Some(crate::Code::ResourceExhausted));
    }

    // ── new_retry_throttler ───────────────────────────────────────────────────

    #[test]
    fn new_retry_throttler_returns_noop_when_no_factory() {
        let _g = lock();
        reset_for_testing();

        let t = new_retry_throttler();
        // noop: throttle always false, callbacks don't panic
        assert!(!t.throttle());
        t.attempt_started(false);
        t.attempt_started(true);
        t.attempt_completed();
    }

    #[test]
    fn new_retry_throttler_calls_factory_when_registered() {
        let _g = lock();
        reset_for_testing();

        let call_count = Arc::new(Mutex::new(0u32));
        let call_count_clone = call_count.clone();

        admin_only_set_custom_retry_throttler(move || {
            *call_count_clone.lock().unwrap() += 1;
            Box::new(NoopThrottler)
        })
        .unwrap();

        new_retry_throttler();
        new_retry_throttler();
        assert_eq!(*call_count.lock().unwrap(), 2);
    }

    #[test]
    fn throttler_callbacks_are_invoked() {
        let _g = lock();
        reset_for_testing();

        #[derive(Default)]
        struct Tracker {
            starts: Mutex<Vec<bool>>,
            completions: Mutex<u32>,
        }
        impl RetryThrottler for Arc<Tracker> {
            fn throttle(&self) -> bool {
                false
            }
            fn attempt_started(&self, is_retry: bool) {
                self.starts.lock().unwrap().push(is_retry);
            }
            fn attempt_completed(&self) {
                *self.completions.lock().unwrap() += 1;
            }
        }

        let tracker = Arc::new(Tracker::default());
        let tracker_for_factory = tracker.clone();
        admin_only_set_custom_retry_throttler(move || Box::new(tracker_for_factory.clone()))
            .unwrap();

        let t = new_retry_throttler();
        t.attempt_started(false);
        t.attempt_started(true);
        t.attempt_completed();
        t.attempt_completed();

        assert_eq!(*tracker.starts.lock().unwrap(), vec![false, true]);
        assert_eq!(*tracker.completions.lock().unwrap(), 2);
    }

    // ── admin_only_set_custom_retry_hook validation ───────────────────────────

    #[test]
    fn set_hook_rejects_invalid_policy() {
        let _g = lock();
        reset_for_testing();

        let bad_policy = RetryPolicy {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
            backoff_multiplier: 2.0,
        };
        assert!(admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, bad_policy).is_err());
    }

    #[test]
    fn set_hook_can_be_overwritten() {
        let _g = lock();
        reset_for_testing();

        admin_only_set_custom_retry_hook(|_| RetryDecision::Retry, valid_policy()).unwrap();
        // Second call overwrites the first (no "set once" restriction in our impl)
        admin_only_set_custom_retry_hook(|_| RetryDecision::NoRetry, valid_policy()).unwrap();

        let status = Status::unavailable("x");
        assert!(try_custom_retry(&status).is_err()); // NoRetry
    }
}
