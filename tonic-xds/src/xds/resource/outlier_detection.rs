//! Outlier-detection configuration types (gRFC A50).
//!
//! These are the validated config inputs consumed by the outlier-detection
//! algorithm. Parsing them from `envoy.config.cluster.v3.OutlierDetection`
//! and exposing them on `ClusterResource` lands in a follow-up PR alongside
//! the wiring into the load-balancing pipeline.
//!
//! [gRFC A50]: https://github.com/grpc/proposal/blob/master/A50-xds-outlier-detection.md

use std::time::Duration;

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
    /// Maximum percentage of endpoints that may be ejected at any time (0-100).
    pub max_ejection_percent: u32,
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
    /// Probability (0-100) that a candidate is actually ejected.
    pub enforcement_percentage: u32,
    /// Minimum number of candidate endpoints required to run the algorithm.
    pub minimum_hosts: u32,
    /// Minimum number of requests an endpoint must have seen in the last
    /// interval to be considered a candidate.
    pub request_volume: u32,
}

/// Failure-percentage ejection parameters (gRFC A50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailurePercentageConfig {
    /// Failure rate (0-100) at or above which an endpoint is a candidate
    /// for ejection.
    pub threshold: u32,
    /// Probability (0-100) that a candidate is actually ejected.
    pub enforcement_percentage: u32,
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
            max_ejection_percent: 10,
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
            enforcement_percentage: 100,
            minimum_hosts: 5,
            request_volume: 100,
        });
        assert!(c.is_enabled());
    }

    #[test]
    fn is_enabled_true_when_failure_percentage_present() {
        let mut c = base_config();
        c.failure_percentage = Some(FailurePercentageConfig {
            threshold: 85,
            enforcement_percentage: 100,
            minimum_hosts: 5,
            request_volume: 50,
        });
        assert!(c.is_enabled());
    }
}
