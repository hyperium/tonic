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

use http::Method;
use http::Response;
use http::StatusCode;
use http::request::Parts;

use super::stream::trailers_only_response;
use crate::core::ConnectionInfo;
use crate::metadata::KeyAndValueRef;
use crate::metadata::MetadataMap;
use crate::server::RequestHeaders;
use crate::server::Trailers;
use crate::status::StatusCodeError;
use crate::status::StatusError;

/// Error returned when an incoming HTTP/2 request fails gRPC protocol or header validation.
///
/// While gRPC `Trailers-Only` errors normally use HTTP `200 OK`, certain HTTP-level
/// protocol violations (such as an invalid HTTP method or unsupported `content-type`)
/// require returning a non-200 HTTP status code (e.g., `405` or `415`) per the
/// gRPC-over-HTTP/2 spec.
#[derive(Debug)]
pub struct RequestValidationError {
    http_status: Option<StatusCode>,
    grpc_status: StatusError,
}

impl RequestValidationError {
    fn new(code: StatusCodeError, message: impl Into<String>) -> Self {
        Self {
            http_status: None,
            grpc_status: StatusError::new(code, message),
        }
    }

    fn with_http_status(mut self, http_status: StatusCode) -> Self {
        self.http_status = Some(http_status);
        self
    }

    /// Converts this validation error into a trailers-only [`Response`] with an empty body.
    pub fn into_http<B: Default>(self) -> Response<B> {
        let mut response = trailers_only_response(Trailers::new(Err(self.grpc_status)));
        if let Some(http_status) = self.http_status {
            *response.status_mut() = http_status;
        }
        response
    }
}

/// Returns true if `ct` is a valid gRPC over HTTP/2 content-type
/// (`application/grpc`, `application/grpc+...`, or `application/grpc;...`).
/// Case-insensitive per RFC 9110.
fn is_valid_grpc_content_type(ct: &str) -> bool {
    const PREFIX: &str = "application/grpc";
    let Some((prefix, rest)) = ct.split_at_checked(PREFIX.len()) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(PREFIX)
        && (rest.is_empty() || rest.starts_with('+') || rest.starts_with(';'))
}

/// Validates that the HTTP/2 request uses `POST` and has a valid gRPC `content-type`.
/// Also rejects any `grpc-encoding` other than `identity`, since this transport
/// can't decompress messages.
fn validate_grpc_request(parts: &Parts) -> Result<(), RequestValidationError> {
    if parts.method != Method::POST {
        return Err(RequestValidationError::new(
            StatusCodeError::Internal,
            format!("invalid HTTP method: {}, expected POST", parts.method),
        )
        .with_http_status(StatusCode::METHOD_NOT_ALLOWED));
    }

    match parts.headers.get("content-type") {
        None => Err(RequestValidationError::new(
            StatusCodeError::Unimplemented,
            "missing content-type header",
        )
        .with_http_status(StatusCode::UNSUPPORTED_MEDIA_TYPE)),
        Some(val) => match val.to_str() {
            Ok(ct) if is_valid_grpc_content_type(ct) => Ok(()),
            Ok(ct) => Err(RequestValidationError::new(
                StatusCodeError::Unimplemented,
                format!("unsupported content-type: {ct}"),
            )
            .with_http_status(StatusCode::UNSUPPORTED_MEDIA_TYPE)),
            Err(_) => Err(RequestValidationError::new(
                StatusCodeError::Unimplemented,
                "invalid non-ASCII content-type header",
            )
            .with_http_status(StatusCode::UNSUPPORTED_MEDIA_TYPE)),
        },
    }?;

    // `te` isn't checked here: h2 rejects any value other than `trailers` while
    // decoding headers, and a missing `te` is tolerated for proxies that strip it.

    // This transport can't decompress messages, so per the gRPC compression
    // spec any encoding other than `identity` is rejected with UNIMPLEMENTED.
    // TODO: Support compression, then accept those encodings here and list them
    // in a `grpc-accept-encoding` response header, as the spec requires.
    if let Some(val) = parts
        .headers
        .get("grpc-encoding")
        .filter(|val| *val != "identity")
    {
        return Err(RequestValidationError::new(
            StatusCodeError::Unimplemented,
            format!(
                "unsupported grpc-encoding: {}",
                String::from_utf8_lossy(val.as_bytes())
            ),
        ));
    }

    Ok(())
}

/// Returns true if `key` is a gRPC-reserved transport framing or status header
/// that must be stripped from application-visible `RequestHeaders::metadata()`
/// and handler-supplied `ResponseHeaders::metadata()`.
///
/// This checks the explicit list of 8 transport-owned headers rather than
/// `key.starts_with("grpc-")` so that gRPC extension headers (such as
/// `grpc-trace-bin`, `grpc-tags-bin`, `grpc-server-stats-bin`,
/// `grpc-previous-rpc-attempts`, and `grpc-retry-pushback-ms`) remain visible
/// to interceptors operating on `MetadataMap` above the transport.
///
/// `grpc-status-details-bin` is intentionally not reserved, so handlers can
/// pass rich error details through, as in grpc-go.
pub fn is_reserved_header(key: &str) -> bool {
    matches!(
        key,
        "te" | "content-type"
            | "grpc-timeout"
            | "grpc-encoding"
            | "grpc-accept-encoding"
            | "grpc-message-type"
            | "grpc-status"
            | "grpc-message"
    )
}

/// Parses the gRPC timeout format (`<1-8 digits>[H|M|S|m|u|n]`) into a [`Duration`].
///
/// Duplicated from `tonic::transport::service::grpc_timeout::try_parse_grpc_timeout`
/// because that function is private to `tonic` and gated behind the `transport`
/// (`server` / `channel`) features, which `grpc` does not enable.
fn parse_grpc_timeout(s: &str) -> Option<Duration> {
    if s.is_empty() {
        return None;
    }
    // `MetadataValue<Ascii>::to_str` guarantees visible ASCII, so `s.len() - 1`
    // is always on a char boundary.
    let (val_str, unit) = s.split_at(s.len() - 1);
    if val_str.len() > 8 {
        return None;
    }
    let val: u64 = val_str.parse().ok()?;
    match unit {
        "H" => Some(Duration::from_secs(val * 3600)),
        "M" => Some(Duration::from_secs(val * 60)),
        "S" => Some(Duration::from_secs(val)),
        "m" => Some(Duration::from_millis(val)),
        "u" => Some(Duration::from_micros(val)),
        "n" => Some(Duration::from_nanos(val)),
        _ => None,
    }
}

/// Validates incoming HTTP/2 request parts and extracts [`RequestHeaders`]
/// (method path, timeout, compression encodings, and filtered metadata).
pub fn extract_request_headers(
    parts: &Parts,
    connection_info: ConnectionInfo,
) -> Result<RequestHeaders, RequestValidationError> {
    validate_grpc_request(parts)?;

    let method_name = parts.uri.path().to_string();
    // TODO: Revisit `MetadataMap::from_headers` behavior for non-ASCII headers:
    // currently invalid ASCII bytes (e.g. obs-text) are silently dropped on the floor
    // rather than returning an error like the `-bin` branch.
    let mut metadata = MetadataMap::from_headers(&parts.headers).map_err(|e| {
        RequestValidationError::new(
            StatusCodeError::Internal,
            format!("error decoding request metadata: {e}"),
        )
    })?;
    // Extract timeout and compression headers onto RequestHeaders before stripping
    // reserved headers from the application-visible MetadataMap.
    // TODO: Revisit symmetry with `client::CallOptions` (whether timeout/deadline
    // and compression encodings should live on `server::RequestHeaders` or `server::CallOptions`).
    let timeout = match metadata.remove("grpc-timeout") {
        Some(val) => {
            let s = val.to_str();
            let dur = parse_grpc_timeout(s).ok_or_else(|| {
                RequestValidationError::new(
                    StatusCodeError::Internal,
                    format!("malformed grpc-timeout header: {s}"),
                )
            })?;
            Some(dur)
        }
        None => None,
    };
    let encoding = metadata.remove("grpc-encoding");
    let accept_encoding = metadata.remove("grpc-accept-encoding");
    metadata.retain(|entry| match entry {
        KeyAndValueRef::Ascii(key, _) => !is_reserved_header(key.as_str()),
        KeyAndValueRef::Binary(key, _) => !is_reserved_header(key.as_str()),
    });

    let mut request_headers =
        RequestHeaders::new(method_name, connection_info).with_metadata(metadata);
    if let Some(dur) = timeout {
        request_headers = request_headers.with_timeout(dur);
    }
    if let Some(enc) = encoding {
        request_headers = request_headers.with_encoding(enc);
    }
    if let Some(acc) = accept_encoding {
        request_headers = request_headers.with_accept_encoding(acc);
    }
    Ok(request_headers)
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use http::Request;

    use super::*;
    use crate::attributes::Attributes;
    use crate::core::Address;
    use crate::credentials::SecurityInfo;
    use crate::credentials::SecurityLevel;

    fn test_connection_info() -> ConnectionInfo {
        let addr = Address {
            network_type: "tcp",
            address: "127.0.0.1:50051".to_string().into(),
            attributes: Attributes::new(),
        };
        ConnectionInfo::new(
            addr.clone(),
            addr,
            SecurityInfo::new("local").with_security_level(SecurityLevel::NoSecurity),
        )
    }

    #[test]
    fn validate_grpc_request_accepts_valid_content_types() {
        for valid_ct in [
            "application/grpc",
            "application/grpc+proto",
            "application/grpc; charset=utf-8",
            "Application/GRPC",
            "Application/GRPC+proto",
            "application/GRPC; charset=utf-8",
        ] {
            let (parts, ()) = Request::builder()
                .method(Method::POST)
                .uri("/test.Service/Method")
                .header("content-type", valid_ct)
                .body(())
                .unwrap()
                .into_parts();
            assert!(validate_grpc_request(&parts).is_ok());
        }
    }

    #[test]
    fn validate_grpc_request_rejects_non_post_method() {
        let (get_parts, ()) = Request::builder()
            .method(Method::GET)
            .uri("/test.Service/Method")
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .body(())
            .unwrap()
            .into_parts();
        let err = validate_grpc_request(&get_parts).unwrap_err();
        assert_eq!(err.http_status, Some(StatusCode::METHOD_NOT_ALLOWED));
        assert_eq!(err.grpc_status.code(), StatusCodeError::Internal);
        assert_eq!(
            err.grpc_status.message(),
            "invalid HTTP method: GET, expected POST"
        );

        let http_resp = err.into_http::<()>();
        assert_eq!(http_resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(http_resp.headers().get("grpc-status").unwrap(), "13");
    }

    #[test]
    fn validate_grpc_request_rejects_missing_content_type() {
        let (missing_ct_parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/Method")
            .header("te", "trailers")
            .body(())
            .unwrap()
            .into_parts();
        let err = validate_grpc_request(&missing_ct_parts).unwrap_err();
        assert_eq!(err.http_status, Some(StatusCode::UNSUPPORTED_MEDIA_TYPE));
        assert_eq!(err.grpc_status.code(), StatusCodeError::Unimplemented);
        assert_eq!(err.grpc_status.message(), "missing content-type header");
    }

    #[test]
    fn validate_grpc_request_rejects_unsupported_content_types() {
        for invalid_ct in ["text/plain", "application/json", "application/grpc-web"] {
            let (parts, ()) = Request::builder()
                .method(Method::POST)
                .uri("/test.Service/Method")
                .header("content-type", invalid_ct)
                .header("te", "trailers")
                .body(())
                .unwrap()
                .into_parts();
            let err = validate_grpc_request(&parts).unwrap_err();
            assert_eq!(err.http_status, Some(StatusCode::UNSUPPORTED_MEDIA_TYPE));
            assert_eq!(err.grpc_status.code(), StatusCodeError::Unimplemented);
            assert_eq!(
                err.grpc_status.message(),
                format!("unsupported content-type: {invalid_ct}")
            );
        }
        assert!(!is_valid_grpc_content_type("application/grp🦀"));
    }

    #[test]
    fn validate_grpc_request_rejects_non_ascii_content_type() {
        let (non_ascii_ct_parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/Method")
            .header(
                "content-type",
                HeaderValue::from_bytes(b"application/grpc\xFF").unwrap(),
            )
            .header("te", "trailers")
            .body(())
            .unwrap()
            .into_parts();
        let err = validate_grpc_request(&non_ascii_ct_parts).unwrap_err();
        assert_eq!(err.http_status, Some(StatusCode::UNSUPPORTED_MEDIA_TYPE));
        assert_eq!(err.grpc_status.code(), StatusCodeError::Unimplemented);
        assert_eq!(
            err.grpc_status.message(),
            "invalid non-ASCII content-type header"
        );
    }

    #[test]
    fn validate_grpc_request_accepts_identity_grpc_encoding() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/Method")
            .header("content-type", "application/grpc")
            .header("grpc-encoding", "identity")
            .body(())
            .unwrap()
            .into_parts();
        assert!(validate_grpc_request(&parts).is_ok());
    }

    #[test]
    fn validate_grpc_request_rejects_unsupported_grpc_encoding() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/Method")
            .header("content-type", "application/grpc")
            .header("grpc-encoding", "gzip")
            .body(())
            .unwrap()
            .into_parts();
        let err = validate_grpc_request(&parts).unwrap_err();
        assert_eq!(err.http_status, None);
        assert_eq!(err.grpc_status.code(), StatusCodeError::Unimplemented);
        assert_eq!(err.grpc_status.message(), "unsupported grpc-encoding: gzip");

        let http_resp = err.into_http::<()>();
        assert_eq!(http_resp.status(), StatusCode::OK);
        assert_eq!(http_resp.headers().get("grpc-status").unwrap(), "12");
    }

    #[test]
    fn is_reserved_header_matches_transport_owned_headers_and_allows_extensions() {
        for reserved in [
            "te",
            "content-type",
            "grpc-timeout",
            "grpc-encoding",
            "grpc-accept-encoding",
            "grpc-message-type",
            "grpc-status",
            "grpc-message",
        ] {
            assert!(is_reserved_header(reserved), "{reserved} must be reserved");
        }
        for allowed in [
            "grpc-status-details-bin",
            "grpc-trace-bin",
            "grpc-tags-bin",
            "grpc-server-stats-bin",
            "grpc-previous-rpc-attempts",
            "grpc-retry-pushback-ms",
            "user-agent",
            "x-custom",
        ] {
            assert!(!is_reserved_header(allowed), "{allowed} must be allowed");
        }
    }

    #[test]
    fn parse_grpc_timeout_valid_units() {
        assert_eq!(parse_grpc_timeout("2H"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_grpc_timeout("5M"), Some(Duration::from_secs(300)));
        assert_eq!(parse_grpc_timeout("10S"), Some(Duration::from_secs(10)));
        assert_eq!(parse_grpc_timeout("250m"), Some(Duration::from_millis(250)));
        assert_eq!(parse_grpc_timeout("500u"), Some(Duration::from_micros(500)));
        assert_eq!(parse_grpc_timeout("100n"), Some(Duration::from_nanos(100)));
        assert_eq!(
            parse_grpc_timeout("99999999S"),
            Some(Duration::from_secs(99_999_999))
        );
    }

    #[test]
    fn parse_grpc_timeout_rejects_malformed_values() {
        assert!(parse_grpc_timeout("").is_none());
        assert!(parse_grpc_timeout("S").is_none());
        assert!(parse_grpc_timeout("100000000S").is_none());
        assert!(parse_grpc_timeout("-5S").is_none());
        assert!(parse_grpc_timeout("oneS").is_none());
        assert!(parse_grpc_timeout("10X").is_none());
    }

    #[test]
    fn extract_request_headers_populates_fields_and_strips_reserved_headers() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("http://localhost/test.Service/InspectHeaders")
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .header("grpc-timeout", "5S")
            .header("grpc-encoding", "identity")
            .header("grpc-accept-encoding", "gzip,identity")
            .header("grpc-message-type", "test.Request")
            .header("grpc-status", "0")
            .header("grpc-message", "ignored")
            .header("grpc-status-details-bin", "AA==")
            .header("grpc-trace-bin", "AA==")
            .header("user-agent", "custom-agent/1.0")
            .header("x-custom-ascii", "allowed-value")
            .body(())
            .unwrap()
            .into_parts();

        let headers = extract_request_headers(&parts, test_connection_info()).unwrap();
        assert_eq!(headers.method_name(), "/test.Service/InspectHeaders");
        assert_eq!(headers.timeout(), Some(Duration::from_secs(5)));
        assert_eq!(headers.encoding(), Some("identity"));
        assert_eq!(headers.accept_encoding(), Some("gzip,identity"));

        let md = headers.metadata();
        assert!(md.get("content-type").is_none());
        assert!(md.get("te").is_none());
        assert!(md.get("grpc-timeout").is_none());
        assert!(md.get("grpc-encoding").is_none());
        assert!(md.get("grpc-accept-encoding").is_none());
        assert!(md.get("grpc-message-type").is_none());
        assert!(md.get("grpc-status").is_none());
        assert!(md.get("grpc-message").is_none());

        assert_eq!(
            md.get_bin("grpc-status-details-bin").map(|v| v.as_bytes()),
            Some(&[0u8][..])
        );
        assert_eq!(
            md.get_bin("grpc-trace-bin").map(|v| v.as_bytes()),
            Some(&[0u8][..])
        );
        assert_eq!(
            md.get("user-agent").map(|v| v.to_str()),
            Some("custom-agent/1.0")
        );
        assert_eq!(
            md.get("x-custom-ascii").map(|v| v.to_str()),
            Some("allowed-value")
        );
    }

    #[test]
    fn extract_request_headers_without_optional_headers() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/Minimal")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap()
            .into_parts();

        let headers = extract_request_headers(&parts, test_connection_info()).unwrap();
        assert_eq!(headers.method_name(), "/test.Service/Minimal");
        assert_eq!(headers.timeout(), None);
        assert_eq!(headers.encoding(), None);
        assert_eq!(headers.accept_encoding(), None);
        assert!(headers.metadata().is_empty());
    }

    #[test]
    fn extract_request_headers_rejects_invalid_request_parts() {
        let (parts, ()) = Request::builder()
            .method(Method::GET)
            .uri("/test.Service/Invalid")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap()
            .into_parts();

        let err = extract_request_headers(&parts, test_connection_info()).unwrap_err();
        assert_eq!(err.http_status, Some(StatusCode::METHOD_NOT_ALLOWED));
        assert_eq!(err.grpc_status.code(), StatusCodeError::Internal);
    }

    #[test]
    fn extract_request_headers_rejects_malformed_binary_metadata() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/CorruptBin")
            .header("content-type", "application/grpc")
            .header("x-corrupt-bin", "!!!not-valid-base64!!!")
            .body(())
            .unwrap()
            .into_parts();

        let err = extract_request_headers(&parts, test_connection_info()).unwrap_err();
        assert_eq!(err.http_status, None);
        assert_eq!(err.grpc_status.code(), StatusCodeError::Internal);
        assert!(
            err.grpc_status
                .message()
                .contains("error decoding request metadata")
        );

        let http_resp = err.into_http::<()>();
        assert_eq!(http_resp.status(), StatusCode::OK);
        assert_eq!(http_resp.headers().get("grpc-status").unwrap(), "13");
    }

    #[test]
    fn extract_request_headers_rejects_malformed_grpc_timeout() {
        let (parts, ()) = Request::builder()
            .method(Method::POST)
            .uri("/test.Service/BadTimeout")
            .header("content-type", "application/grpc")
            .header("grpc-timeout", "invalid")
            .body(())
            .unwrap()
            .into_parts();

        let err = extract_request_headers(&parts, test_connection_info()).unwrap_err();
        assert_eq!(err.http_status, None);
        assert_eq!(err.grpc_status.code(), StatusCodeError::Internal);
        assert_eq!(
            err.grpc_status.message(),
            "malformed grpc-timeout header: invalid"
        );
    }
}
