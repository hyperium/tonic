use crate::metadata::map::MetadataMap;
use http::header::HeaderValue;
use std::{str::FromStr, string::ToString, time::Duration};

/// Internal struct used to convert from a [`std::time::Duration`] to a `"grpc-timeout"` `String`
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct GrpcTimeout {
    deadline: Duration,
}

impl GrpcTimeout {
    pub(crate) fn into_inner(self) -> Duration {
        self.deadline
    }
}

impl From<Duration> for GrpcTimeout {
    fn from(deadline: Duration) -> GrpcTimeout {
        GrpcTimeout { deadline }
    }
}

impl ToString for GrpcTimeout {
    fn to_string(&self) -> String {
        format!("{}n", self.deadline.as_nanos())
    }
}

impl From<GrpcTimeout> for HeaderValue {
    fn from(timeout: GrpcTimeout) -> HeaderValue {
        HeaderValue::from_str(&timeout.to_string())
            .expect("failed to create HeaderValue from GrpcTimeout!")
    }
}

impl std::convert::TryFrom<&MetadataMap> for GrpcTimeout {
    type Error = ();

    fn try_from(map: &MetadataMap) -> Result<GrpcTimeout, Self::Error> {
        map.get("grpc-timeout")
            .map(|s| GrpcTimeout::from_str(s.to_str().unwrap()).ok())
            .flatten()
            .ok_or(())
    }
}

impl FromStr for GrpcTimeout {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let idx = s.find("n").unwrap();
        let (value, _unit) = s.split_at(idx);
        let nanos: u64 = value.parse().unwrap();

        Ok(GrpcTimeout {
            deadline: Duration::from_nanos(nanos),
        })
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
