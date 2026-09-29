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

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Maximum `seconds` value allowed by `google.protobuf.Duration`.
const MAX_PROTO_DURATION_SECONDS: u64 = 315_576_000_000;

/// File-based certificate configuration shared by the A29 file-watcher
/// provider and A65 ADS channel credentials.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct TlsChannelCredentials {
    /// Path to PEM X.509 identity certificate or certificate chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) certificate_file: Option<PathBuf>,
    /// Path to PEM PKCS private key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) private_key_file: Option<PathBuf>,
    /// Path to PEM X.509 CA trust bundle (root certificates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ca_certificate_file: Option<PathBuf>,
    /// How often to re-read the files. Default: 600s.
    #[serde(
        default,
        deserialize_with = "deserialize_proto_duration",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_proto_duration"
    )]
    pub(crate) refresh_interval: Option<Duration>,
}

impl TlsChannelCredentials {
    /// Creates A65 TLS credentials that use system roots and no client identity.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the PEM CA bundle used to validate the xDS server.
    #[must_use]
    pub fn with_ca_certificate_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_certificate_file = Some(path.into());
        self
    }

    /// Sets the PEM certificate chain and private key used for ADS mTLS.
    #[must_use]
    pub fn with_identity_files(
        mut self,
        certificate_file: impl Into<PathBuf>,
        private_key_file: impl Into<PathBuf>,
    ) -> Self {
        self.certificate_file = Some(certificate_file.into());
        self.private_key_file = Some(private_key_file.into());
        self
    }

    /// Sets how often certificate files are refreshed.
    #[must_use]
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = Some(interval);
        self
    }

    #[cfg_attr(not(feature = "_tls-any"), allow(dead_code))]
    pub(crate) fn has_certificate_files(&self) -> bool {
        self.certificate_file.is_some()
            || self.private_key_file.is_some()
            || self.ca_certificate_file.is_some()
    }

    pub(crate) fn has_paired_identity_files(&self) -> bool {
        self.certificate_file.is_some() == self.private_key_file.is_some()
    }
}

pub(crate) type FileWatcherConfig = TlsChannelCredentials;

/// Deserialize a protobuf JSON duration string (e.g., `"60s"`, `"0.5s"`).
fn deserialize_proto_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(s) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let duration = parse_proto_duration(&s).map_err(serde::de::Error::custom)?;
    if duration.is_zero() {
        return Err(serde::de::Error::custom(format!(
            "invalid duration '{s}': must be greater than 0"
        )));
    }
    Ok(Some(duration))
}

fn parse_proto_duration(s: &str) -> Result<Duration, String> {
    let value = s
        .strip_suffix('s')
        .ok_or_else(|| format!("invalid duration '{s}': must end with 's'"))?;
    let (seconds, fraction) = match value.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (value, None),
    };

    if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "invalid duration '{s}': seconds must contain only decimal digits"
        ));
    }
    let seconds: u64 = seconds
        .parse()
        .map_err(|error| format!("invalid duration '{s}': invalid seconds: {error}"))?;
    if seconds > MAX_PROTO_DURATION_SECONDS {
        return Err(format!(
            "invalid duration '{s}': seconds must not exceed {MAX_PROTO_DURATION_SECONDS}"
        ));
    }

    let nanos = match fraction {
        Some(fraction) => {
            if fraction.is_empty() {
                return Err(format!(
                    "invalid duration '{s}': fractional seconds must not be empty"
                ));
            }
            if fraction.len() > 9 {
                return Err(format!(
                    "invalid duration '{s}': fractional seconds must not exceed 9 digits"
                ));
            }
            if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(format!(
                    "invalid duration '{s}': fractional seconds must contain only decimal digits"
                ));
            }
            let scale = 9 - fraction.len() as u32;
            let fraction_value: u32 = fraction
                .parse()
                .map_err(|error| format!("invalid duration '{s}': invalid fraction: {error}"))?;
            fraction_value * 10u32.pow(scale)
        }
        None => 0,
    };

    Ok(Duration::new(seconds, nanos))
}

fn serialize_proto_duration<S>(
    duration: &Option<Duration>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let duration = duration.expect("skipped when absent");
    let seconds = duration.as_secs();
    let nanos = duration.subsec_nanos();
    if nanos == 0 {
        serializer.serialize_str(&format!("{seconds}s"))
    } else {
        let fraction = format!("{nanos:09}").trim_end_matches('0').to_string();
        serializer.serialize_str(&format!("{seconds}.{fraction}s"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: serde_json::Value) -> Result<FileWatcherConfig, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn parses_refresh_interval() {
        for (value, expected) in [
            ("60s", Duration::from_secs(60)),
            ("0.5s", Duration::from_millis(500)),
            ("1.000000001s", Duration::new(1, 1)),
            (
                "315576000000.999999999s",
                Duration::new(MAX_PROTO_DURATION_SECONDS, 999_999_999),
            ),
        ] {
            let config = parse(serde_json::json!({"refresh_interval": value})).unwrap();
            assert_eq!(config.refresh_interval, Some(expected), "{value}");
        }
    }

    #[test]
    fn absent_refresh_interval_is_none() {
        let config = parse(serde_json::json!({})).unwrap();
        assert_eq!(config.refresh_interval, None);
    }

    #[test]
    fn rejects_non_positive_refresh_interval() {
        for value in ["0s", "0.000000000s", "-1s"] {
            assert!(
                parse(serde_json::json!({"refresh_interval": value})).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_non_protobuf_duration_syntax() {
        for value in [
            "60",
            "60ms",
            "1e3s",
            ".5s",
            "1.s",
            "1.0000000000s",
            "315576000001s",
            "NaNs",
            "infs",
        ] {
            assert!(
                parse(serde_json::json!({"refresh_interval": value})).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn serializes_refresh_interval_without_losing_precision() {
        let config =
            TlsChannelCredentials::new().with_refresh_interval(Duration::new(1, 100_000_001));
        assert_eq!(
            serde_json::to_value(config).unwrap()["refresh_interval"],
            "1.100000001s"
        );
    }
}
