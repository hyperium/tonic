/*
 *
 * Copyright 2025 gRPC authors.
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

//! Validated configuration types for [gRFC A32] circuit breaking.
//!
//! gRPC supports only the `max_requests` threshold from Envoy's CDS
//! `CircuitBreakers` config. Other threshold fields are intentionally ignored
//! because they are connection-pool or retry specific and do not apply to gRPC's
//! A32 request limiter.
//!
//! `ClusterResource` carries this config to the client-side limiter.
//!
//! [gRFC A32]: https://github.com/grpc/proposal/blob/master/A32-xds-circuit-breaking.md

use envoy_types::pb::envoy::config::cluster::v3::{CircuitBreakers, circuit_breakers::Thresholds};
use envoy_types::pb::envoy::config::core::v3::RoutingPriority;

/// Default max concurrent requests per cluster from A32.
pub(crate) const DEFAULT_MAX_REQUESTS: u32 = 1024;

/// Validated A32 circuit-breaking configuration for a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CircuitBreakingConfig {
    /// Maximum number of in-flight requests allowed for the upstream cluster.
    ///
    /// The client-side limiter holds requests against this threshold for the
    /// full response-body lifetime.
    pub(crate) max_requests: u32,
}

impl CircuitBreakingConfig {
    /// Build circuit-breaking config from a CDS `CircuitBreakers` message.
    ///
    /// A32 uses the first threshold for `RoutingPriority::Default`. If no
    /// applicable threshold or `max_requests` value is present, the gRPC default
    /// of 1024 is used.
    pub(crate) fn from_proto(circuit_breakers: Option<&CircuitBreakers>) -> Self {
        let max_requests = circuit_breakers
            .and_then(first_default_threshold)
            .and_then(|threshold| threshold.max_requests.as_ref())
            .map(|value| value.value)
            .unwrap_or(DEFAULT_MAX_REQUESTS);

        Self { max_requests }
    }
}

impl Default for CircuitBreakingConfig {
    fn default() -> Self {
        Self {
            max_requests: DEFAULT_MAX_REQUESTS,
        }
    }
}

fn first_default_threshold(circuit_breakers: &CircuitBreakers) -> Option<&Thresholds> {
    circuit_breakers.thresholds.iter().find(|threshold| {
        RoutingPriority::try_from(threshold.priority).ok() == Some(RoutingPriority::Default)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use envoy_types::pb::google::protobuf::UInt32Value;

    fn threshold(priority: RoutingPriority, max_requests: Option<u32>) -> Thresholds {
        Thresholds {
            priority: priority as i32,
            max_requests: max_requests.map(|value| UInt32Value { value }),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_when_circuit_breakers_absent() {
        assert_eq!(
            CircuitBreakingConfig::from_proto(None),
            CircuitBreakingConfig {
                max_requests: DEFAULT_MAX_REQUESTS,
            }
        );
    }

    #[test]
    fn defaults_when_default_threshold_absent() {
        let circuit_breakers = CircuitBreakers {
            thresholds: vec![threshold(RoutingPriority::High, Some(7))],
            ..Default::default()
        };

        assert_eq!(
            CircuitBreakingConfig::from_proto(Some(&circuit_breakers)).max_requests,
            DEFAULT_MAX_REQUESTS
        );
    }

    #[test]
    fn defaults_when_default_threshold_has_no_max_requests() {
        let circuit_breakers = CircuitBreakers {
            thresholds: vec![threshold(RoutingPriority::Default, None)],
            ..Default::default()
        };

        assert_eq!(
            CircuitBreakingConfig::from_proto(Some(&circuit_breakers)).max_requests,
            DEFAULT_MAX_REQUESTS
        );
    }

    #[test]
    fn uses_first_default_threshold_max_requests() {
        let circuit_breakers = CircuitBreakers {
            thresholds: vec![
                threshold(RoutingPriority::High, Some(9)),
                threshold(RoutingPriority::Default, Some(11)),
                threshold(RoutingPriority::Default, Some(13)),
            ],
            ..Default::default()
        };

        assert_eq!(
            CircuitBreakingConfig::from_proto(Some(&circuit_breakers)).max_requests,
            11
        );
    }
}
