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

use std::time::Duration;

use serde::Deserialize;

// Wraps std::time::Duration to provide custom serialization and deserialization
// for gRPC service config, according to the protobuf Duration format.
#[derive(PartialEq, Debug, Clone)]
pub(crate) struct GrpcDuration(pub(crate) Duration);

impl From<GrpcDuration> for Duration {
    fn from(d: GrpcDuration) -> Self {
        d.0
    }
}

impl From<Duration> for GrpcDuration {
    fn from(d: Duration) -> Self {
        GrpcDuration(d)
    }
}

impl std::ops::Deref for GrpcDuration {
    type Target = Duration;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Service Config uses the [protobuf Duration format](https://protobuf.dev/reference/protobuf/google.protobuf/#duration)
/// when parsing. The format is a string with a number followed by an optional
/// fraction and the letter 's' for seconds. For example, "1s", "1.5s",
/// "0.000000001s" are valid durations.
impl<'de> Deserialize<'de> for GrpcDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw_value = String::deserialize(deserializer)?;
        let duration = parse_duration(&raw_value).map_err(serde::de::Error::custom)?;
        Ok(GrpcDuration(duration))
    }
}

const MAX_DURATION_SECONDS: u64 = 315_576_000_000;

// Parsing logic, isolated for testing and to reduce serde monomorphization
// footprint.
fn parse_duration(s: &str) -> Result<Duration, String> {
    if !s.ends_with('s') || s.contains('+') {
        return Err("invalid duration string".to_string());
    }
    let s = &s[..s.len() - 1]; // strip 's'.
    let mut parts = s.splitn(2, '.');
    let secs_str = parts
        .next()
        .ok_or_else(|| "empty duration string".to_string())?;
    let secs: u64 = secs_str
        .parse()
        .map_err(|e| format!("failed to parse seconds: {e}"))?;
    if secs > MAX_DURATION_SECONDS {
        return Err(format!("duration exceeds max of {MAX_DURATION_SECONDS}s"));
    }

    let nanos = if let Some(fraction_str) = parts.next() {
        if fraction_str.is_empty() {
            return Err("empty fraction part".to_string());
        }
        if fraction_str.len() > 9 {
            return Err("fraction part has more than 9 digits".to_string());
        }
        let fraction_val: u32 = fraction_str
            .parse()
            .map_err(|e| format!("failed to parse fraction: {e}"))?;
        let pad = u32::try_from(9 - fraction_str.len())
            .map_err(|e| format!("invalid fraction length: {e}"))?;
        fraction_val * 10u32.pow(pad)
    } else {
        0
    };
    Ok(Duration::new(secs, nanos))
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_duration("0s").unwrap(), Duration::from_secs(0));
        assert_eq!(
            parse_duration("315576000000s").unwrap(),
            Duration::from_secs(315_576_000_000)
        );
        assert_eq!(
            parse_duration("1.5s").unwrap(),
            Duration::new(1, 500_000_000)
        );
        assert_eq!(parse_duration("0.000000001s").unwrap(), Duration::new(0, 1));
        assert_eq!(parse_duration("1.0000001s").unwrap(), Duration::new(1, 100));
        assert_eq!(
            parse_duration("1.0001s").unwrap(),
            Duration::new(1, 100_000)
        );

        assert!(parse_duration("").is_err());
        assert!(parse_duration("1").is_err());
        assert!(parse_duration("1.s").is_err());
        assert!(parse_duration("1.0000000001s").is_err());
        assert!(parse_duration("as").is_err());
        assert!(parse_duration(".5s").is_err());
        assert!(parse_duration("+1s").is_err());
        assert!(parse_duration("1.+5s").is_err());
        assert!(parse_duration("315576000001s").is_err());
    }

    #[test]
    fn test_deserialize_duration() {
        let test_cases = [
            ("1s", GrpcDuration(Duration::from_secs(1))),
            ("0s", GrpcDuration(Duration::from_secs(0))),
            ("1.5s", GrpcDuration(Duration::new(1, 500_000_000))),
            ("0.000000001s", GrpcDuration(Duration::new(0, 1))),
            ("1.0000001s", GrpcDuration(Duration::new(1, 100))),
            ("1.0001s", GrpcDuration(Duration::new(1, 100_000))),
        ];
        for (s, expected) in test_cases {
            let val: GrpcDuration = serde_json::from_value(json!(s)).unwrap();
            assert_eq!(val, expected); // Deserialized value is correct.
        }
    }
}
