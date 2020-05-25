use http::header::HeaderValue;
use std::{string::ToString, time::Duration};

/// Internal struct used to convert from a [`std::time::Duration`] to a `"grpc-timeout"` `String`
#[derive(Debug)]
pub(crate) struct GrpcTimeout<'a> {
    deadline: &'a Duration,
}

impl<'a> From<&'a Duration> for GrpcTimeout<'a> {
    fn from(deadline: &'a Duration) -> GrpcTimeout<'a> {
        GrpcTimeout { deadline }
    }
}

impl<'a> ToString for GrpcTimeout<'a> {
    fn to_string(&self) -> String {
        format!("{}n", self.deadline.as_nanos())
    }
}

impl<'a> From<GrpcTimeout<'a>> for HeaderValue {
    fn from(timeout: GrpcTimeout<'a>) -> HeaderValue {
        HeaderValue::from_str(&timeout.to_string())
            .expect("failed to create HeaderValue from GrpcTimeout!")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_converts_to_correct_header_value() {
        let duration = Duration::new(1, 500);
        let timeout = GrpcTimeout::from(&duration);
        let header_value: HeaderValue = timeout.into();

        assert_eq!(HeaderValue::from_static("1000000500n"), header_value);
    }

}
