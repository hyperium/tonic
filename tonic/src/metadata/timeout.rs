use crate::{metadata::map::MetadataMap, Status};
use futures_util::{FutureExt, future::Either};
use http::header::HeaderValue;
use std::{future::Future, str::FromStr, string::ToString, time::Duration};

const GRPC_TIMEOUT_HEADER_CODE: &str = "grpc-timeout";

/// Internal struct used to convert from a [`std::time::Duration`] to a `"grpc-timeout"` `String`
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct GrpcTimeout {
    deadline: Duration,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum GrpcTimeoutError {
    Parse,
    NotPresent,
}

impl GrpcTimeout {
    pub(crate) fn try_read_from_metadata(metadata: &MetadataMap) -> Result<Self, GrpcTimeoutError> {
        let value = metadata
            .get(GRPC_TIMEOUT_HEADER_CODE)
            .ok_or(GrpcTimeoutError::NotPresent)?
            .to_str()
            .map_err(|_| GrpcTimeoutError::Parse)?;
        GrpcTimeout::from_str(value)
    }
}

impl From<Duration> for GrpcTimeout {
    fn from(deadline: Duration) -> GrpcTimeout {
        GrpcTimeout { deadline }
    }
}

impl From<GrpcTimeout> for Duration {
    fn from(timeout: GrpcTimeout) -> Duration {
        timeout.deadline
    }
}

impl ToString for GrpcTimeout {
    fn to_string(&self) -> String {
        // TODO: Smarter conversion from Duration to "grpc-timeout", don't just always convert to nanos
        format!("{}n", self.deadline.as_nanos())
    }
}

impl From<GrpcTimeout> for HeaderValue {
    fn from(timeout: GrpcTimeout) -> HeaderValue {
        HeaderValue::from_str(&timeout.to_string())
            .expect("failed to create HeaderValue from GrpcTimeout!")
    }
}

impl FromStr for GrpcTimeout {
    type Err = GrpcTimeoutError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let idx = s.find("n").ok_or(GrpcTimeoutError::Parse)?;
        let (value, _unit) = s.split_at(idx);
        let nanos: u64 = value.parse().map_err(|_| GrpcTimeoutError::Parse)?;

        Ok(GrpcTimeout {
            deadline: Duration::from_nanos(nanos),
        })
    }
}

pub(crate) fn wrap_with_timeout<R>(
    future: impl Future<Output = Result<R, Status>>,
    deadline: Option<GrpcTimeout>,
) -> impl Future<Output = Result<R, Status>> {
    match deadline {
        Some(d) => {
            let duration = d.into();
            Either::Left(tokio::time::timeout(duration, future).map(|timeout_result| match timeout_result {
                Ok(resp) => resp,
                Err(_) => Err(Status::cancelled("request timed out!")),
            }))
        },
        None => Either::Right(future),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_converts_to_correct_header_value() {
        let duration = Duration::new(1, 500);
        let timeout = GrpcTimeout::from(duration);
        let header_value: HeaderValue = timeout.into();

        assert_eq!(HeaderValue::from_static("1000000500n"), header_value);
    }

    #[test]
    fn str_converts_to_timeout() {
        let str_timeout = GrpcTimeout::from_str("1000000500n").unwrap();
        let timeout = GrpcTimeout {
            deadline: Duration::new(1, 500),
        };
        assert_eq!(str_timeout, timeout);
    }
}
