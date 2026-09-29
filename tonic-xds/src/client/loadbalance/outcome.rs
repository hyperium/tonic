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

//! Transport-agnostic outcome classification for outlier detection.
//!
//! The load balancer records a per-call success/failure signal into outlier
//! detection. What counts as a failure is transport-specific, so the decision
//! is delegated to a pluggable [`OutcomeClassifier`] — mirroring the retry
//! layer's [`RetryClassifier`](crate::RetryClassifier). The built-in
//! [`GrpcOutcomeClassifier`] interprets gRPC status; a non-gRPC transport (e.g.
//! plain HTTP) supplies its own to interpret HTTP status codes without touching
//! the outlier-detection engine.
//!
//! The seam is crate-internal for now: nothing outside the crate can inject a
//! classifier yet. The public builder hook (and the final `#[non_exhaustive]`
//! shaping of these types) lands with the outlier-detection `Discover` wiring.

use std::fmt::Debug;

/// Borrowed view of one endpoint call's result, handed to
/// [`OutcomeClassifier::classify`]. It carries no error payload — outlier
/// detection only needs to know that a call errored, not why — so it works with
/// the load balancer's generic endpoint error type.
#[derive(Debug)]
pub(crate) enum CallOutcome<'a> {
    /// The endpoint produced a response; inspect `status`/`headers` (e.g. gRPC's
    /// trailers-only `grpc-status`) to decide.
    Response {
        /// The response status.
        status: http::StatusCode,
        /// The response headers.
        headers: &'a http::HeaderMap,
    },
    /// The endpoint failed before producing a usable response (e.g. a
    /// connection-level error).
    Error,
}

/// The health verdict for a single call, recorded by outlier detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealthOutcome {
    /// Count the call as a success.
    Success,
    /// Count the call as a failure.
    Failure,
    /// Do not record the call — it reflects neither upstream health nor fault
    /// (e.g. HTTP `4xx` client errors).
    Ignore,
}

/// Maps a transport [`CallOutcome`] to a [`HealthOutcome`] for outlier
/// detection.
///
/// This is the transport seam for OD accounting: the built-in
/// [`GrpcOutcomeClassifier`] interprets gRPC status, while a non-gRPC transport
/// (e.g. plain HTTP) supplies its own to interpret HTTP status codes (`5xx` =
/// failure, `4xx` = ignore, ...) without touching the outlier-detection engine.
pub(crate) trait OutcomeClassifier: Debug + Send + Sync + 'static {
    /// Classify a single endpoint call outcome.
    fn classify(&self, outcome: CallOutcome<'_>) -> HealthOutcome;
}

/// gRPC outcome classifier. A transport error or a non-2xx HTTP status is a
/// failure; on a 2xx response the `grpc-status` in the leading headers decides.
/// Decoding is deferred to [`tonic::Status::from_header_map`] so the verdict
/// matches how the RPC actually resolves — e.g. `grpc-status: 00` is not a valid
/// `0` and tonic maps it to `Unknown`, a failure. An absent `grpc-status`
/// (anything that isn't trailers-only) is treated as a success.
#[derive(Debug, Default, Clone)]
pub(crate) struct GrpcOutcomeClassifier;

impl OutcomeClassifier for GrpcOutcomeClassifier {
    fn classify(&self, outcome: CallOutcome<'_>) -> HealthOutcome {
        match outcome {
            CallOutcome::Error => HealthOutcome::Failure,
            CallOutcome::Response { status, headers } => {
                if !status.is_success() {
                    return HealthOutcome::Failure;
                }
                match tonic::Status::from_header_map(headers) {
                    None => HealthOutcome::Success,
                    Some(s) if s.code() == tonic::Code::Ok => HealthOutcome::Success,
                    Some(_) => HealthOutcome::Failure,
                }
            }
        }
    }
}

/// Extracts a [`CallOutcome`] from an endpoint response so the load balancer can
/// classify outcomes while staying generic over the response body. Implemented
/// for [`http::Response`]; the whole xDS data plane is HTTP-typed.
pub(crate) trait OutcomeSource {
    /// Borrow this response as a [`CallOutcome`].
    fn call_outcome(&self) -> CallOutcome<'_>;
}

impl<B> OutcomeSource for http::Response<B> {
    fn call_outcome(&self) -> CallOutcome<'_> {
        CallOutcome::Response {
            status: self.status(),
            headers: self.headers(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, grpc_status: Option<&str>) -> http::Response<()> {
        let mut builder = http::Response::builder().status(status);
        if let Some(code) = grpc_status {
            builder = builder.header("grpc-status", code);
        }
        builder.body(()).unwrap()
    }

    fn classify(status: u16, grpc_status: Option<&str>) -> HealthOutcome {
        GrpcOutcomeClassifier.classify(response(status, grpc_status).call_outcome())
    }

    #[test]
    fn grpc_transport_error_is_failure() {
        assert_eq!(
            GrpcOutcomeClassifier.classify(CallOutcome::Error),
            HealthOutcome::Failure
        );
    }

    #[test]
    fn grpc_ok_without_status_is_success() {
        assert_eq!(classify(200, None), HealthOutcome::Success);
    }

    #[test]
    fn grpc_trailers_only_zero_is_success() {
        assert_eq!(classify(200, Some("0")), HealthOutcome::Success);
    }

    #[test]
    fn grpc_trailers_only_nonzero_is_failure() {
        // 13 = INTERNAL.
        assert_eq!(classify(200, Some("13")), HealthOutcome::Failure);
    }

    #[test]
    fn grpc_unparseable_status_is_failure() {
        assert_eq!(classify(200, Some("not-a-number")), HealthOutcome::Failure);
    }

    #[test]
    fn grpc_leading_zero_status_is_failure() {
        // "00" is not a valid grpc-status `0`; tonic decodes it to Unknown, so
        // the RPC fails and OD must not count it as a success.
        assert_eq!(classify(200, Some("00")), HealthOutcome::Failure);
    }

    #[test]
    fn grpc_non_2xx_is_failure() {
        assert_eq!(classify(503, None), HealthOutcome::Failure);
    }
}
