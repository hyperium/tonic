//! Validated configuration types for [gRFC A50] outlier detection.
//!
//! [`OutlierDetectionConfig`] is the input to the outlier-detection
//! algorithm. The two sub-configs gate which ejection algorithms run.
//!
//! Note: A50 specifies outlier detection as a load-balancing policy
//! wrapping a `child_policy`. `tonic-xds` currently runs P2C as its
//! only load balancer, so there is no `child_policy` field here yet —
//! it will be added when more balancers are supported. Integration
//! with the data path is via an mpsc channel of ejection decisions
//! polled by the [`LoadBalancer`] tower service, which marks the
//! corresponding [`ReadyChannel`] as ejected via [`EjectedChannel`].
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md
//! [`LoadBalancer`]: crate::client::loadbalance::loadbalancer::LoadBalancer
//! [`ReadyChannel`]: crate::client::loadbalance::channel_state::ReadyChannel
//! [`EjectedChannel`]: crate::client::loadbalance::channel_state::EjectedChannel

use std::time::Duration;

/// A 0–100 percentage. Construction is fallible; once held, every
/// `Percentage` is guaranteed to be in range, so the algorithm never
/// has to re-validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Percentage(u8);

impl Percentage {
    /// Construct from a raw value, returning `Err` if it exceeds 100.
    /// Accepts `u32` to match the proto wire type without forcing callers
    /// to cast at every site.
    pub(crate) fn new(value: u32) -> Result<Self, PercentageError> {
        if value > 100 {
            Err(PercentageError(value))
        } else {
            Ok(Self(value as u8))
        }
    }

    /// The contained value, in `0..=100`.
    pub(crate) fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("percentage must be in 0..=100, got {0}")]
pub(crate) struct PercentageError(u32);

/// Validated A50 outlier-detection configuration for a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutlierDetectionConfig {
    /// How often the ejection sweep runs.
    pub interval: Duration,
    /// Base duration for a single ejection; actual ejection time is
    /// `base_ejection_time * multiplier`, capped by `max_ejection_time`.
    pub base_ejection_time: Duration,
    /// Upper bound on `base_ejection_time * multiplier`. The spec guarantees
    /// this is at least `base_ejection_time`.
    pub max_ejection_time: Duration,
    /// Maximum percentage of endpoints that may be ejected at any time.
    pub max_ejection_percent: Percentage,
    /// Success-rate ejection parameters. `None` if the algorithm is disabled.
    pub success_rate: Option<SuccessRateConfig>,
    /// Failure-percentage ejection parameters. `None` if the algorithm is
    /// disabled.
    pub failure_percentage: Option<FailurePercentageConfig>,
}

/// Success-rate ejection parameters (gRFC A50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SuccessRateConfig {
    /// Ejection threshold factor, scaled by 1000 (so `1900` means `1.9`).
    /// An endpoint is a candidate for ejection when its success rate falls
    /// below `mean - stdev * (stdev_factor / 1000.0)`.
    pub stdev_factor: u32,
    /// Probability that a flagged candidate is actually ejected — *not*
    /// the success-rate threshold (which is derived from `stdev_factor`).
    /// Set to 0 to disable enforcement while still computing statistics.
    pub enforcing_success_rate: Percentage,
    /// Minimum number of candidate endpoints required to run the algorithm.
    pub minimum_hosts: u32,
    /// Minimum number of requests an endpoint must have seen in the last
    /// interval to be considered a candidate.
    pub request_volume: u32,
}

/// Failure-percentage ejection parameters (gRFC A50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailurePercentageConfig {
    /// Failure rate at or above which an endpoint is a candidate for
    /// ejection.
    pub threshold: Percentage,
    /// Probability that a flagged candidate is actually ejected — *not*
    /// the failure-rate threshold (that is `threshold` above). Set to 0
    /// to disable enforcement while still computing statistics.
    pub enforcing_failure_percentage: Percentage,
    /// Minimum number of candidate endpoints required to run the algorithm.
    pub minimum_hosts: u32,
    /// Minimum number of requests an endpoint must have seen in the last
    /// interval to be considered a candidate.
    pub request_volume: u32,
}

impl OutlierDetectionConfig {
    /// True when at least one ejection algorithm is enabled and the detector
    /// should do work. If false, the cluster can skip instantiating detection.
    pub(crate) fn is_enabled(&self) -> bool {
        self.success_rate.is_some() || self.failure_percentage.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> OutlierDetectionConfig {
        OutlierDetectionConfig {
            interval: Duration::from_secs(10),
            base_ejection_time: Duration::from_secs(30),
            max_ejection_time: Duration::from_secs(300),
            max_ejection_percent: Percentage::new(10).unwrap(),
            success_rate: None,
            failure_percentage: None,
        }
    }

    #[test]
    fn is_enabled_false_when_both_algorithms_disabled() {
        assert!(!base_config().is_enabled());
    }

    #[test]
    fn is_enabled_true_when_success_rate_present() {
        let mut c = base_config();
        c.success_rate = Some(SuccessRateConfig {
            stdev_factor: 1900,
            enforcing_success_rate: Percentage::new(100).unwrap(),
            minimum_hosts: 5,
            request_volume: 100,
        });
        assert!(c.is_enabled());
    }

    #[test]
    fn is_enabled_true_when_failure_percentage_present() {
        let mut c = base_config();
        c.failure_percentage = Some(FailurePercentageConfig {
            threshold: Percentage::new(85).unwrap(),
            enforcing_failure_percentage: Percentage::new(100).unwrap(),
            minimum_hosts: 5,
            request_volume: 50,
        });
        assert!(c.is_enabled());
    }

    #[test]
    fn percentage_accepts_zero_to_one_hundred() {
        for v in [0, 1, 50, 99, 100] {
            assert_eq!(Percentage::new(v).unwrap().get() as u32, v);
        }
    }

    #[test]
    fn percentage_rejects_values_above_one_hundred() {
        assert_eq!(Percentage::new(101), Err(PercentageError(101)));
        assert_eq!(Percentage::new(u32::MAX), Err(PercentageError(u32::MAX)));
    }
}
