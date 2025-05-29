/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

use rand::Rng;
use std::{sync::Mutex, time::Duration};

#[derive(Clone)]
pub struct BackoffConfig {
    /// The amount of time to backoff after the first failure.
    pub base_delay: Duration,

    /// The factor with which to multiply backoffs after a
    /// failed retry. Should ideally be greater than 1.
    pub multiplier: f64,

    /// The factor with which backoffs are randomized.
    pub jitter: f64,

    /// The upper bound of backoff delay.
    pub max_delay: Duration,
}

pub struct ExponentialBackoff {
    config: BackoffConfig,

    /// The delay for the next retry, without the random jitter. Store as f64
    /// to avoid rounding errors.
    next_delay_secs: Mutex<f64>,
}

/// This is a backoff configuration with the default values specified
/// at https://github.com/grpc/grpc/blob/master/doc/connection-backoff.md.
///
/// This should be useful for callers who want to configure backoff with
/// non-default values only for a subset of the options.
pub const DEFAULT_EXPONENTIAL_CONFIG: BackoffConfig = BackoffConfig {
    base_delay: Duration::from_secs(1),
    multiplier: 1.6,
    jitter: 0.2,
    max_delay: Duration::from_secs(120),
};

impl BackoffConfig {
    fn validate(&self) -> Result<(), &'static str> {
        // Valid that params are in valid ranges.
        // 0 <= base_dealy <= max_delay
        if self.base_delay > self.max_delay {
            Err("base_delay must be greater than max_delay")?;
        }
        // 1 <= multiplier
        if self.multiplier < 1.0 {
            Err("multiplier must be greater than 1.0")?;
        }
        // 0 <= jitter <= 1
        if self.jitter < 0.0 {
            Err("jitter must be greater than or equal to 0")?;
        }
        if self.jitter > 1.0 {
            Err("jitter must be less than or equal to 1")?
        }
        Ok(())
    }
}

impl ExponentialBackoff {
    pub fn new(config: BackoffConfig) -> Result<Self, &'static str> {
        config.validate()?;
        let next_delay_secs = config.base_delay.as_secs_f64();
        Ok(ExponentialBackoff {
            config,
            next_delay_secs: Mutex::new(next_delay_secs),
        })
    }

    pub fn reset(&self) {
        let mut next_delay = self.next_delay_secs.lock().unwrap();
        *next_delay = self.config.base_delay.as_secs_f64();
    }

    pub fn backoff_duration(&self) -> Duration {
        let mut next_delay = self.next_delay_secs.lock().unwrap();
        let cur_delay =
            *next_delay * (1.0 + self.config.jitter * rand::thread_rng().gen_range(-1.0..1.0));
        *next_delay = self
            .config
            .max_delay
            .as_secs_f64()
            .min(*next_delay * self.config.multiplier);
        Duration::from_secs_f64(cur_delay)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::client::name_resolution::backoff::{
        BackoffConfig, ExponentialBackoff, DEFAULT_EXPONENTIAL_CONFIG,
    };

    // Epsilon for floating point comparisons if needed, though Duration
    // comparisons are often better.
    const EPSILON: f64 = 1e-9;

    #[test]
    fn default_config_is_valid() {
        let result = ExponentialBackoff::new(DEFAULT_EXPONENTIAL_CONFIG.clone());
        assert_eq!(result.is_ok(), true);
    }

    #[test]
    fn base_less_than_max() {
        let config = BackoffConfig {
            base_delay: Duration::from_secs(10),
            multiplier: 123.0,
            jitter: 0.0,
            max_delay: Duration::from_secs(100),
        };
        let backoff = ExponentialBackoff::new(config).unwrap();
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(10));
    }

    #[test]
    fn base_more_than_max() {
        let config = BackoffConfig {
            multiplier: 123.0,
            jitter: 0.0,
            base_delay: Duration::from_secs(100),
            max_delay: Duration::from_secs(10),
        };
        let result = ExponentialBackoff::new(config);
        assert_eq!(result.is_err(), true);
    }

    #[test]
    fn negative_multiplier() {
        let config = BackoffConfig {
            multiplier: -123.0,
            jitter: 0.0,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(100),
        };
        let result = ExponentialBackoff::new(config);
        assert_eq!(result.is_err(), true);
    }

    #[test]
    fn negative_jitter() {
        let config = BackoffConfig {
            multiplier: 1.0,
            jitter: -10.0,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(100),
        };
        let result = ExponentialBackoff::new(config);
        assert_eq!(result.is_err(), true);
    }

    #[test]
    fn jitter_greater_than_one() {
        let config = BackoffConfig {
            multiplier: 1.0,
            jitter: 2.0,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(100),
        };
        let result = ExponentialBackoff::new(config);
        assert_eq!(result.is_err(), true);
    }

    #[test]
    fn backoff_reset_no_jitter() {
        let config = BackoffConfig {
            multiplier: 2.0,
            jitter: 0.0,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(15),
        };
        let backoff = ExponentialBackoff::new(config.clone()).unwrap();
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(1));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(2));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(4));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(8));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(15));
        // Duration is capped to max_delay.
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(15));

        // reset and repeat.
        backoff.reset();
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(1));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(2));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(4));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(8));
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(15));
        // Duration is capped to max_delay.
        assert_eq!(backoff.backoff_duration(), Duration::from_secs(15));
    }

    #[test]
    fn backoff_with_jitter() {
        let config = BackoffConfig {
            multiplier: 2.0,
            jitter: 0.2,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(15),
        };
        let backoff = ExponentialBackoff::new(config.clone()).unwrap();
        // 0.8 <= duration <= 1.2.
        let duration = backoff.backoff_duration();
        assert_eq!(duration.gt(&Duration::from_secs_f64(0.8 - EPSILON)), true);
        assert_eq!(duration.lt(&Duration::from_secs_f64(1.2 + EPSILON)), true);
        // 1.6 <= duration <= 2.4.
        let duration = backoff.backoff_duration();
        assert_eq!(duration.gt(&Duration::from_secs_f64(1.6 - EPSILON)), true);
        assert_eq!(duration.lt(&Duration::from_secs_f64(2.4 + EPSILON)), true);
        // 3.2 <= duration <= 4.8.
        let duration = backoff.backoff_duration();
        assert_eq!(duration.gt(&Duration::from_secs_f64(3.2 - EPSILON)), true);
        assert_eq!(duration.lt(&Duration::from_secs_f64(4.8 + EPSILON)), true);
    }
}
