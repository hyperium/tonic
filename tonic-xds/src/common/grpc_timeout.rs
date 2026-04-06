//! gRPC timeout header parsing utilities.

use std::time::Duration;

/// Errors that can occur when parsing a gRPC timeout header value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum GrpcTimeoutParseError<'a> {
    /// The header value is empty.
    #[error("empty timeout header")]
    Empty,
    /// The header format is invalid (should not happen for non-empty ASCII strings).
    #[error("invalid timeout format: {0}")]
    InvalidFormat(&'a str),
    /// The value portion is not a valid integer.
    #[error("non-digit timeout value")]
    NonDigitValue,
    /// The unit character is not one of the valid gRPC timeout units.
    #[error("invalid timeout unit: {0}")]
    InvalidUnit(char),
    /// Arithmetic overflow when converting to Duration.
    #[error("timeout value overflow")]
    Overflow,
}

/// Parse a gRPC timeout header value (e.g. "1S", "500m", "100u").
/// Format per gRPC spec: `<value><unit>` where unit is one of
/// H (hours), M (minutes), S (seconds), m (millis), u (micros), n (nanos).
pub(crate) fn parse_grpc_timeout(s: &str) -> Result<Duration, GrpcTimeoutParseError<'_>> {
    if s.is_empty() {
        return Err(GrpcTimeoutParseError::Empty);
    }
    let (digits, unit) = s
        .split_at_checked(s.len() - 1)
        .ok_or(GrpcTimeoutParseError::InvalidFormat(s))?;
    let value: u64 = digits
        .parse()
        .map_err(|_| GrpcTimeoutParseError::NonDigitValue)?;
    let unit_char = unit
        .chars()
        .next()
        .ok_or(GrpcTimeoutParseError::InvalidFormat(s))?;
    match unit_char {
        'H' => Ok(Duration::from_secs(
            value
                .checked_mul(3600)
                .ok_or(GrpcTimeoutParseError::Overflow)?,
        )),
        'M' => Ok(Duration::from_secs(
            value
                .checked_mul(60)
                .ok_or(GrpcTimeoutParseError::Overflow)?,
        )),
        'S' => Ok(Duration::from_secs(value)),
        'm' => Ok(Duration::from_millis(value)),
        'u' => Ok(Duration::from_micros(value)),
        'n' => Ok(Duration::from_nanos(value)),
        _ => Err(GrpcTimeoutParseError::InvalidUnit(unit_char)),
    }
}

/// Extract the timeout from a request's `grpc-timeout` header.
/// Returns `None` if the header is missing, non-ASCII, or cannot be parsed.
pub(crate) fn extract_grpc_timeout<B>(req: &http::Request<B>) -> Option<Duration> {
    let header_value = req.headers().get("grpc-timeout")?;
    let s = header_value.to_str().ok()?;
    parse_grpc_timeout(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Happy path tests ---

    #[test]
    fn test_hours() {
        assert_eq!(
            parse_grpc_timeout("3H").unwrap(),
            Duration::from_secs(3 * 3600)
        );
    }

    #[test]
    fn test_minutes() {
        assert_eq!(parse_grpc_timeout("1M").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_seconds() {
        assert_eq!(parse_grpc_timeout("42S").unwrap(), Duration::from_secs(42));
    }

    #[test]
    fn test_milliseconds() {
        assert_eq!(
            parse_grpc_timeout("13m").unwrap(),
            Duration::from_millis(13)
        );
    }

    #[test]
    fn test_microseconds() {
        assert_eq!(parse_grpc_timeout("2u").unwrap(), Duration::from_micros(2));
    }

    #[test]
    fn test_nanoseconds() {
        assert_eq!(parse_grpc_timeout("82n").unwrap(), Duration::from_nanos(82));
    }

    // --- Error path tests ---

    #[test]
    fn test_empty_header() {
        assert_eq!(parse_grpc_timeout(""), Err(GrpcTimeoutParseError::Empty));
    }

    #[test]
    fn test_header_without_digits() {
        // A single character like "H" has empty digit portion → NonDigitValue
        assert_eq!(
            parse_grpc_timeout("H"),
            Err(GrpcTimeoutParseError::NonDigitValue)
        );
    }

    #[test]
    fn test_non_digit_value() {
        assert_eq!(
            parse_grpc_timeout("abcS"),
            Err(GrpcTimeoutParseError::NonDigitValue)
        );
    }

    #[test]
    fn test_invalid_unit() {
        assert_eq!(
            parse_grpc_timeout("82f"),
            Err(GrpcTimeoutParseError::InvalidUnit('f'))
        );
    }

    #[test]
    fn test_overflow_hours() {
        let big = format!("{}H", u64::MAX);
        assert_eq!(
            parse_grpc_timeout(&big),
            Err(GrpcTimeoutParseError::Overflow)
        );
    }

    #[test]
    fn test_overflow_minutes() {
        let big = format!("{}M", u64::MAX);
        assert_eq!(
            parse_grpc_timeout(&big),
            Err(GrpcTimeoutParseError::Overflow)
        );
    }

    // --- extract_grpc_timeout tests ---

    #[test]
    fn test_extract_missing_header() {
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(extract_grpc_timeout(&req), None);
    }

    #[test]
    fn test_extract_valid_header() {
        let req = http::Request::builder()
            .header("grpc-timeout", "5S")
            .body(())
            .unwrap();
        assert_eq!(extract_grpc_timeout(&req), Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_extract_invalid_header() {
        let req = http::Request::builder()
            .header("grpc-timeout", "badvalue")
            .body(())
            .unwrap();
        assert_eq!(extract_grpc_timeout(&req), None);
    }
}
