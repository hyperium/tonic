use http::Request;
use std::{
    task::{Context, Poll},
    time::Duration,
};
use tower_service::Service;
use tracing::{debug, warn};

pub(crate) struct Timeout<S> {
    inner: S,
    server_timeout: Option<Duration>,
}

impl<S> Timeout<S> {
    pub(crate) fn new(inner: S, server_timeout: Option<Duration>) -> Self {
        Self {
            inner,
            server_timeout,
        }
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for Timeout<S>
where
    S: Service<Request<ReqBody>>,
    S::Error: Into<crate::Error>,
{
    type Response = S::Response;
    type Error = crate::Error;
    type Future = future::TimeoutFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Try to parse the `grpc-timeout` header, if it is present
        let header_timeout = headers::try_parse_grpc_timeout(req.headers()).unwrap_or_else(|e| {
            warn!("Error parsing grpc-timeout header {:?}", e);
            None
        });

        // Use the shorter of the two durations, if either are set
        let timeout_duration = match (header_timeout, self.server_timeout) {
            (None, None) => None,
            (Some(dur), None) => Some(dur),
            (None, Some(dur)) => Some(dur),
            (Some(header), Some(server)) => {
                let shorter_duration = std::cmp::min(header, server);
                debug!(
                    "both grpc-timeout header present: {:?},\
                     and server timeout set: {:?}.\
                     Using server timeout of: {:?}",
                    header, server, shorter_duration,
                );
                Some(shorter_duration)
            }
        };

        let inner_future = self.inner.call(req);
        future::TimeoutFuture::new(inner_future, timeout_duration)
    }
}

/// Utility methods for parsing the gRPC headers
mod headers {
    use http::{HeaderMap, HeaderValue};
    use std::time::Duration;

    const GRPC_TIMEOUT_HEADER: &str = "grpc-timeout";

    const SECONDS_IN_HOUR: u64 = 60 * 60;
    const SECONDS_IN_MINUTE: u64 = 60;

    /// Tries to parse the `grpc-timeout` header if it is present. If we fail to parse, returns
    /// the value we attempted to parse.
    ///
    /// Follows the [gRPC over HTTP2 spec](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md).
    pub(crate) fn try_parse_grpc_timeout(
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<Option<Duration>, &HeaderValue> {
        match headers.get(GRPC_TIMEOUT_HEADER) {
            Some(val) => {
                let str_val = val.to_str().map_err(|_| val)?;
                let (timeout_value, timeout_unit) = try_split_last(str_val).map_err(|_| val)?;
                let timeout_value: u64 = timeout_value.parse().map_err(|_| val)?;

                let duration = match timeout_unit {
                    // Hours
                    "H" => Duration::from_secs(timeout_value * SECONDS_IN_HOUR),
                    // Minutes
                    "M" => Duration::from_secs(timeout_value * SECONDS_IN_MINUTE),
                    // Seconds
                    "S" => Duration::from_secs(timeout_value),
                    // Milliseconds
                    "m" => Duration::from_millis(timeout_value),
                    // Microseconds
                    "u" => Duration::from_micros(timeout_value),
                    // Nanoseconds
                    "n" => Duration::from_nanos(timeout_value),
                    _ => return Err(val),
                };

                Ok(Some(duration))
            }
            None => Ok(None),
        }
    }

    /// Tries to split the last character of the string, from the rest of the string,
    /// returning (rest_of_string, last_char), if successful.
    ///
    /// `str.split_at(...)` panics if we're not on a UTF-8 code point boundary. This
    /// should never happen in practice because the `grpc-timeout` header should be only
    /// ASCII characters.
    fn try_split_last(val: &str) -> Result<(&str, &str), &str> {
        std::panic::catch_unwind(|| val.split_at(val.len() - 1)).map_err(|_| val)
    }
}

/// A custom error type that the Timeout Service returns.
mod error {
    use std::{fmt, time::Duration};

    // Note: The wrapped Duration should only be used for logging purposes. It is **not** the
    // actual duration that elapsed, resulting in a timeout, instead it is a close approximation
    #[derive(Debug)]
    pub(crate) struct TimeoutExpired(pub(crate) Duration);

    impl fmt::Display for TimeoutExpired {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "Timeout expired after {:?}", self.0)
        }
    }

    // std::error::Error only requires a type to impl Debug and Display
    impl std::error::Error for TimeoutExpired {}
}

/// A Future that returns `T`, if it resolves before a provided `Duration`
mod future {
    use super::error::TimeoutExpired;
    use pin_project::pin_project;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
        time::{Duration, Instant},
    };
    use tokio::time::{delay_for, Delay};

    #[pin_project(project = TimeoutFutureProj)]
    #[derive(Debug)]
    pub(crate) enum TimeoutFuture<T> {
        NoOp(#[pin] T),
        Timeout {
            #[pin]
            inner: T,
            #[pin]
            timeout: Delay,
            log_start: Instant,
        },
    }

    impl<T> TimeoutFuture<T> {
        pub(crate) fn new(inner: T, duration: Option<Duration>) -> Self {
            match duration {
                Some(dur) => {
                    // Create a Future that resolves after duration
                    let timeout = delay_for(dur);
                    // Record the current instant as when the Future starts, used for logging
                    let log_start = Instant::now();

                    TimeoutFuture::Timeout {
                        inner,
                        timeout,
                        log_start,
                    }
                }
                None => TimeoutFuture::NoOp(inner),
            }
        }
    }

    impl<F, T, E> Future for TimeoutFuture<F>
    where
        F: Future<Output = Result<T, E>>,
        E: Into<crate::Error>,
    {
        type Output = Result<T, crate::Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.project() {
                TimeoutFutureProj::NoOp(inner) => inner.poll(cx).map_err(Into::into),
                TimeoutFutureProj::Timeout {
                    inner,
                    timeout,
                    log_start,
                } => {
                    // Poll our inner future, returning the result if it's ready
                    if let Poll::Ready(output) = inner.poll(cx) {
                        return Poll::Ready(output.map_err(Into::into));
                    };

                    // Poll the timeout, returning an error if it's already resolved
                    match timeout.poll(cx) {
                        Poll::Pending => Poll::Pending,
                        Poll::Ready(_) => {
                            Poll::Ready(Err(Box::new(TimeoutExpired(log_start.elapsed()))))
                        }
                    }
                }
            }
        }
    }
}
