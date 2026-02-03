//! Retry policy configuration based on gRFC A6.

use std::time::Duration;

use crate::error::{Error, Result};

/// Retry policy for xDS client connection attempts.
///
/// This configuration follows the gRFC A6 proposal for client retries,
/// using exponential backoff with jitter for reconnection attempts.
///
/// # Example
///
/// ```
/// use xds_client::RetryPolicy;
/// use std::time::Duration;
///
/// let policy = RetryPolicy::default()
///     .with_initial_backoff(Duration::from_secs(1)).unwrap()
///     .with_max_backoff(Duration::from_secs(30)).unwrap()
///     .with_backoff_multiplier(2.0).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Initial backoff duration for the first retry attempt.
    ///
    /// Default: 1 second.
    pub initial_backoff: Duration,

    /// Maximum backoff duration.
    ///
    /// The backoff will not grow beyond this value, regardless of how many
    /// retry attempts have been made.
    ///
    /// Default: 30 seconds.
    pub max_backoff: Duration,

    /// Multiplier for exponential backoff.
    ///
    /// After each failed attempt, the current backoff duration is multiplied
    /// by this value (up to `max_backoff`).
    ///
    /// Default: 2.0 (exponential backoff).
    pub backoff_multiplier: f64,

    /// Maximum number of retry attempts.
    ///
    /// If `None`, retries indefinitely. If `Some(n)`, stops after `n` attempts.
    ///
    /// Default: None (infinite retries).
    pub max_attempts: Option<usize>,
}

impl RetryPolicy {
    /// Create a new retry policy with custom parameters.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `backoff_multiplier` is less than 1.0
    /// - `max_backoff` is less than `initial_backoff`
    /// - `initial_backoff` is zero
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::RetryPolicy;
    /// use std::time::Duration;
    ///
    /// let policy = RetryPolicy::new(
    ///     Duration::from_millis(500),  // initial_backoff
    ///     Duration::from_secs(60),     // max_backoff
    ///     1.5,                         // backoff_multiplier
    /// )?;
    /// # Ok::<(), xds_client::Error>(())
    /// ```
    pub fn new(
        initial_backoff: Duration,
        max_backoff: Duration,
        backoff_multiplier: f64,
    ) -> Result<Self> {
        if initial_backoff.is_zero() {
            return Err(Error::Validation(
                "initial_backoff must be greater than zero".into(),
            ));
        }

        if backoff_multiplier < 1.0 {
            return Err(Error::Validation(format!(
                "backoff_multiplier must be >= 1.0, got {backoff_multiplier}"
            )));
        }

        if max_backoff < initial_backoff {
            return Err(Error::Validation(format!(
                "max_backoff ({max_backoff:?}) must be >= initial_backoff ({initial_backoff:?})"
            )));
        }

        Ok(Self {
            initial_backoff,
            max_backoff,
            backoff_multiplier,
            max_attempts: None,
        })
    }

    /// Set the initial backoff duration.
    ///
    /// # Errors
    ///
    /// Returns an error if `duration` is zero or greater than `max_backoff`.
    pub fn with_initial_backoff(mut self, duration: Duration) -> Result<Self> {
        if duration.is_zero() {
            return Err(Error::Validation(
                "initial_backoff must be greater than zero".into(),
            ));
        }
        if duration > self.max_backoff {
            let max_backoff = self.max_backoff;
            return Err(Error::Validation(format!(
                "initial_backoff ({duration:?}) must be <= max_backoff ({max_backoff:?})"
            )));
        }
        self.initial_backoff = duration;
        Ok(self)
    }

    /// Set the maximum backoff duration.
    ///
    /// # Errors
    ///
    /// Returns an error if `duration` is less than `initial_backoff`.
    pub fn with_max_backoff(mut self, duration: Duration) -> Result<Self> {
        if duration < self.initial_backoff {
            let initial_backoff = self.initial_backoff;
            return Err(Error::Validation(format!(
                "max_backoff ({duration:?}) must be >= initial_backoff ({initial_backoff:?})"
            )));
        }
        self.max_backoff = duration;
        Ok(self)
    }

    /// Set the backoff multiplier.
    ///
    /// # Errors
    ///
    /// Returns an error if `multiplier` is less than 1.0.
    pub fn with_backoff_multiplier(mut self, multiplier: f64) -> Result<Self> {
        if multiplier < 1.0 {
            return Err(Error::Validation(format!(
                "backoff_multiplier must be >= 1.0, got {multiplier}"
            )));
        }
        self.backoff_multiplier = multiplier;
        Ok(self)
    }

    /// Set the maximum number of retry attempts.
    ///
    /// If set to `None`, retries indefinitely.
    pub fn with_max_attempts(mut self, max_attempts: Option<usize>) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Calculate the backoff duration for a given attempt number.
    ///
    /// Returns `None` if `max_attempts` is set and the attempt exceeds it.
    ///
    /// # Arguments
    ///
    /// * `attempt` - The retry attempt number (0-indexed).
    ///
    /// # Example
    ///
    /// ```
    /// use xds_client::RetryPolicy;
    /// use std::time::Duration;
    ///
    /// let policy = RetryPolicy::default();
    /// assert_eq!(policy.backoff_duration(0), Some(Duration::from_secs(1)));
    /// assert_eq!(policy.backoff_duration(1), Some(Duration::from_secs(2)));
    /// assert_eq!(policy.backoff_duration(2), Some(Duration::from_secs(4)));
    /// ```
    pub fn backoff_duration(&self, attempt: usize) -> Option<Duration> {
        // Check if we've exceeded max attempts
        if let Some(max) = self.max_attempts {
            if attempt >= max {
                return None;
            }
        }

        // Calculate exponential backoff
        let multiplier = self.backoff_multiplier.powi(attempt as i32);
        let backoff = self.initial_backoff.mul_f64(multiplier);

        // Cap at max_backoff
        Some(backoff.min(self.max_backoff))
    }
}

impl Default for RetryPolicy {
    /// Create a retry policy with default values based on gRFC A6.
    ///
    /// Defaults:
    /// - `initial_backoff`: 1 second
    /// - `max_backoff`: 30 seconds
    /// - `backoff_multiplier`: 2.0
    /// - `max_attempts`: None (infinite retries)
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            max_attempts: None,
        }
    }
}

/// Stateful backoff calculator based on a [`RetryPolicy`].
///
/// This struct tracks the current attempt number and provides methods to
/// get the next backoff duration and reset after successful operations.
///
/// # Example
///
/// ```
/// use xds_client::{Backoff, RetryPolicy};
/// use std::time::Duration;
///
/// let mut backoff = Backoff::new(RetryPolicy::default());
///
/// // First failure: get initial backoff
/// assert_eq!(backoff.next_backoff(), Some(Duration::from_secs(1)));
///
/// // Second failure: backoff doubles
/// assert_eq!(backoff.next_backoff(), Some(Duration::from_secs(2)));
///
/// // Success: reset for next failure sequence
/// backoff.reset();
/// assert_eq!(backoff.next_backoff(), Some(Duration::from_secs(1)));
/// ```
#[derive(Debug, Clone)]
pub struct Backoff {
    policy: RetryPolicy,
    attempt: usize,
}

impl Backoff {
    /// Create a new backoff calculator from a retry policy.
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy, attempt: 0 }
    }

    /// Get the next backoff duration and advance the attempt counter.
    ///
    /// Returns `None` if `max_attempts` is set and has been exceeded.
    pub fn next_backoff(&mut self) -> Option<Duration> {
        let duration = self.policy.backoff_duration(self.attempt)?;
        self.attempt += 1;
        Some(duration)
    }

    /// Reset the backoff after a successful operation.
    ///
    /// This resets the attempt counter to 0, so the next failure will
    /// use the initial backoff duration.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}
