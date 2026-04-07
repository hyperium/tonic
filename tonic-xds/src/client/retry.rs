//! gRPC retry utilities.

use std::io;

/// Check if an error's source chain contains a retryable connection-level error.
///
/// These are errors where the request was definitely **not** sent, making it safe to retry.
/// Walks the full error source chain via [`std::error::Error::source`].
pub(crate) fn is_retryable_connection_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io_err) = e.downcast_ref::<io::Error>() {
            match io_err.kind() {
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::NotConnected
                | io::ErrorKind::AddrInUse
                | io::ErrorKind::AddrNotAvailable => return true,
                _ => {}
            }
        }
        current = e.source();
    }
    false
}

/// Check if a gRPC status code is retryable according to the given policy.
pub(crate) fn is_retryable_grpc_status_code(
    code: tonic::Code,
    retryable_codes: &[tonic::Code],
) -> bool {
    retryable_codes.contains(&code)
}

/// Check if a request should be retried, either because of a retryable connection error
/// or because the gRPC response status code is in the retryable set.
pub(crate) fn is_retryable<E: std::error::Error + 'static>(
    result: &Result<&http::Response<()>, &E>,
    retryable_codes: &[tonic::Code],
) -> bool {
    match result {
        Err(err) => is_retryable_connection_error(*err),
        Ok(response) => {
            let status = tonic::Status::from_header_map(response.headers());
            match status {
                Some(status) => is_retryable_grpc_status_code(status.code(), retryable_codes),
                // No grpc-status header means success
                None => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    // --- is_retryable_connection_error tests ---

    #[test]
    fn test_connection_refused_is_retryable() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        assert!(is_retryable_connection_error(&err));
    }

    #[test]
    fn test_not_connected_is_retryable() {
        let err = io::Error::new(io::ErrorKind::NotConnected, "not connected");
        assert!(is_retryable_connection_error(&err));
    }

    #[test]
    fn test_addr_in_use_is_retryable() {
        let err = io::Error::new(io::ErrorKind::AddrInUse, "addr in use");
        assert!(is_retryable_connection_error(&err));
    }

    #[test]
    fn test_addr_not_available_is_retryable() {
        let err = io::Error::new(io::ErrorKind::AddrNotAvailable, "addr not available");
        assert!(is_retryable_connection_error(&err));
    }

    #[test]
    fn test_connection_reset_is_not_retryable() {
        // Connection reset means the request may have been sent
        let err = io::Error::new(io::ErrorKind::ConnectionReset, "reset");
        assert!(!is_retryable_connection_error(&err));
    }

    #[test]
    fn test_timeout_is_not_retryable() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "timed out");
        assert!(!is_retryable_connection_error(&err));
    }

    #[test]
    fn test_nested_connection_refused_is_retryable() {
        // tonic::Status wraps the inner error and exposes it via source()
        let inner = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let mut status = tonic::Status::unavailable("connection refused");
        status.set_source(Arc::new(inner));
        assert!(is_retryable_connection_error(&status));
    }

    #[test]
    fn test_non_io_error_is_not_retryable() {
        #[derive(Debug)]
        struct CustomError;
        impl std::fmt::Display for CustomError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "custom")
            }
        }
        impl std::error::Error for CustomError {}

        assert!(!is_retryable_connection_error(&CustomError));
    }

    // --- is_retryable_grpc_status_code tests ---

    #[test]
    fn test_unavailable_is_retryable() {
        let codes = vec![tonic::Code::Unavailable, tonic::Code::Cancelled];
        assert!(is_retryable_grpc_status_code(
            tonic::Code::Unavailable,
            &codes
        ));
    }

    #[test]
    fn test_ok_is_not_retryable() {
        let codes = vec![tonic::Code::Unavailable, tonic::Code::Cancelled];
        assert!(!is_retryable_grpc_status_code(tonic::Code::Ok, &codes));
    }

    #[test]
    fn test_empty_retryable_codes() {
        assert!(!is_retryable_grpc_status_code(
            tonic::Code::Unavailable,
            &[]
        ));
    }

    // --- is_retryable tests ---

    #[test]
    fn test_is_retryable_connection_error_via_result() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let result: Result<&http::Response<()>, &io::Error> = Err(&err);
        assert!(is_retryable(&result, &[]));
    }

    #[test]
    fn test_is_retryable_grpc_status_via_result() {
        let response = http::Response::builder()
            .header("grpc-status", "14") // UNAVAILABLE
            .body(())
            .unwrap();
        let result: Result<&http::Response<()>, &io::Error> = Ok(&response);
        assert!(is_retryable(&result, &[tonic::Code::Unavailable]));
    }

    #[test]
    fn test_is_not_retryable_ok_response() {
        let response = http::Response::builder()
            .header("grpc-status", "0") // OK
            .body(())
            .unwrap();
        let result: Result<&http::Response<()>, &io::Error> = Ok(&response);
        assert!(!is_retryable(&result, &[tonic::Code::Unavailable]));
    }

    #[test]
    fn test_is_not_retryable_no_grpc_status_header() {
        let response = http::Response::builder().body(()).unwrap();
        let result: Result<&http::Response<()>, &io::Error> = Ok(&response);
        assert!(!is_retryable(&result, &[tonic::Code::Unavailable]));
    }
}
