//! gRPC timeout header parsing utilities.

use std::time::Duration;

/// Errors that can occur when parsing a gRPC timeout header value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GrpcTimeoutParseError {
    /// The header value is empty.
    #[error("empty timeout header")]
    Empty,
    /// The header format is invalid (should not happen for non-empty ASCII strings).
    #[error("invalid timeout format: {0}")]
    InvalidFormat(String),
    /// The value portion is not a valid integer.
    #[error("non-digit timeout value")]
    NonDigitValue,
    /// The unit character is not one of the valid gRPC timeout units.
    #[error("invalid timeout unit: {0}")]
    InvalidUnit(char),
    /// The header value contains non-ASCII bytes.
    #[error("non-ASCII timeout header: {0}")]
    TimeoutNotAscii(http::header::ToStrError),
    /// The timeout value exceeds the gRPC spec limit of 8 digits.
    #[error("timeout value too big")]
    TimeoutValueTooBig,
}

/// gRPC spec: TimeoutValue is a positive integer as ASCII string of at most 8 digits.
const GRPC_TIMEOUT_VALUE_MAX_LEN: usize = 8;

/// Parse a gRPC timeout header value (e.g. "1S", "500m", "100u").
/// Format per gRPC spec: `<value><unit>` where unit is one of
/// H (hours), M (minutes), S (seconds), m (millis), u (micros), n (nanos).
pub(crate) fn parse_grpc_timeout(s: &str) -> Result<Duration, GrpcTimeoutParseError> {
    if s.is_empty() {
        return Err(GrpcTimeoutParseError::Empty);
    }
    // 8 digit value + 1 unit char
    if s.len() > GRPC_TIMEOUT_VALUE_MAX_LEN + 1 {
        return Err(GrpcTimeoutParseError::TimeoutValueTooBig);
    }
    let (digits, unit) = s
        .split_at_checked(s.len() - 1)
        .ok_or_else(|| GrpcTimeoutParseError::InvalidFormat(s.to_owned()))?;
    let value: u64 = digits
        .parse()
        .map_err(|_| GrpcTimeoutParseError::NonDigitValue)?;
    let unit_char = unit
        .chars()
        .next()
        .ok_or_else(|| GrpcTimeoutParseError::InvalidFormat(s.to_owned()))?;
    match unit_char {
        'H' => Ok(Duration::from_secs(value * 3600)),
        'M' => Ok(Duration::from_secs(value * 60)),
        'S' => Ok(Duration::from_secs(value)),
        'm' => Ok(Duration::from_millis(value)),
        'u' => Ok(Duration::from_micros(value)),
        'n' => Ok(Duration::from_nanos(value)),
        _ => Err(GrpcTimeoutParseError::InvalidUnit(unit_char)),
    }
}

/// Extract the timeout from a request's `grpc-timeout` header.
///
/// Returns `Ok(None)` if the header is absent,
/// `Ok(Some(duration))` on success, or `Err` if the header is present but malformed.
pub(crate) fn extract_grpc_timeout<B>(
    req: &http::Request<B>,
) -> Result<Option<Duration>, GrpcTimeoutParseError> {
    let Some(header_value) = req.headers().get("grpc-timeout") else {
        return Ok(None);
    };
    let s = header_value
        .to_str()
        .map_err(GrpcTimeoutParseError::TimeoutNotAscii)?;
    parse_grpc_timeout(s).map(Some)
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
        assert!(matches!(
            parse_grpc_timeout(""),
            Err(GrpcTimeoutParseError::Empty)
        ));
    }

    #[test]
    fn test_header_without_digits() {
        // A single character like "H" has empty digit portion → NonDigitValue
        assert!(matches!(
            parse_grpc_timeout("H"),
            Err(GrpcTimeoutParseError::NonDigitValue)
        ));
    }

    #[test]
    fn test_non_digit_value() {
        assert!(matches!(
            parse_grpc_timeout("abcS"),
            Err(GrpcTimeoutParseError::NonDigitValue)
        ));
    }

    #[test]
    fn test_invalid_unit() {
        assert!(matches!(
            parse_grpc_timeout("82f"),
            Err(GrpcTimeoutParseError::InvalidUnit('f'))
        ));
    }

    #[test]
    fn test_non_ascii_splits_at_utf8_boundary() {
        // "5§" is 3 bytes [0x35, 0xC2, 0xA7]; len-1=2 lands inside the § char,
        // so split_at_checked returns None → InvalidFormat
        assert!(matches!(
            parse_grpc_timeout("5§"),
            Err(GrpcTimeoutParseError::InvalidFormat(_))
        ));
    }

    #[test]
    fn test_timeout_value_too_big() {
        // 9 digits exceeds the gRPC spec limit of 8
        assert!(matches!(
            parse_grpc_timeout("123456789H"),
            Err(GrpcTimeoutParseError::TimeoutValueTooBig)
        ));
    }

    #[test]
    fn test_max_8_digit_value() {
        // 8 digits is the max allowed
        assert_eq!(
            parse_grpc_timeout("99999999S").unwrap(),
            Duration::from_secs(99999999)
        );
    }

    // --- extract_grpc_timeout tests ---

    #[test]
    fn test_extract_missing_header() {
        let req = http::Request::builder().body(()).unwrap();
        assert_eq!(extract_grpc_timeout(&req).unwrap(), None);
    }

    #[test]
    fn test_extract_valid_header() {
        let req = http::Request::builder()
            .header("grpc-timeout", "5S")
            .body(())
            .unwrap();
        assert_eq!(
            extract_grpc_timeout(&req).unwrap(),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn test_extract_non_ascii_header() {
        let mut req = http::Request::builder().body(()).unwrap();
        req.headers_mut().insert(
            "grpc-timeout",
            http::HeaderValue::from_bytes(b"5\xc0S").unwrap(),
        );
        assert!(matches!(
            extract_grpc_timeout(&req),
            Err(GrpcTimeoutParseError::TimeoutNotAscii(_))
        ));
    }

    #[test]
    fn test_extract_invalid_header() {
        let req = http::Request::builder()
            .header("grpc-timeout", "badvalue")
            .body(())
            .unwrap();
        assert!(matches!(
            extract_grpc_timeout(&req),
            Err(GrpcTimeoutParseError::NonDigitValue)
        ));
    }
}
