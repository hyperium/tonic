use std::convert::TryFrom;
use std::time;

use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::pb;
use super::super::{FromAny, IntoAny};

/// Used to encode/decode the `RetryInfo` standard error message described in
/// [error_details.proto]. Describes when the clients can retry a failed
/// request.  
/// Note: When obtained from decoding `RetryInfo` messages, negative
/// `retry_delay`'s become 0.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct RetryInfo {
    /// Informs the amout of time that clients should wait before retrying.
    pub retry_delay: Option<time::Duration>,
}

impl RetryInfo {
    /// Type URL of the `RetryInfo` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.RetryInfo";

    /// Should not exceed `prost_types::Duration` range. Limited to
    /// approximately 10,000 years.
    pub const MAX_RETRY_DELAY: time::Duration = time::Duration::new(315_576_000_000, 999_999_999);

    /// Creates a new [`RetryInfo`] struct. If `retry_delay` exceeds
    /// [`RetryInfo::MAX_RETRY_DELAY`], [`RetryInfo::MAX_RETRY_DELAY`] will
    /// be used instead.
    pub fn new(retry_delay: Option<time::Duration>) -> Self {
        let retry_delay = match retry_delay {
            Some(mut delay) => {
                if delay > RetryInfo::MAX_RETRY_DELAY {
                    delay = RetryInfo::MAX_RETRY_DELAY
                }
                Some(delay)
            }
            None => None,
        };
        RetryInfo {
            retry_delay: retry_delay,
        }
    }
}

impl RetryInfo {
    /// Returns `true` if [`RetryInfo`]'s `retry_delay` is set as `None`, and
    /// `false` if it is not.
    pub fn is_empty(&self) -> bool {
        self.retry_delay.is_none()
    }
}

impl IntoAny for RetryInfo {
    fn into_any(self) -> Any {
        let retry_delay = match self.retry_delay {
            Some(duration) => {
                // If duration is too large, uses max `prost_types::Duration`
                let duration = match prost_types::Duration::try_from(duration) {
                    Ok(duration) => duration,
                    Err(_) => prost_types::Duration {
                        seconds: 315_576_000_000,
                        nanos: 999_999_999,
                    },
                };
                Some(duration)
            }
            None => None,
        };

        let detail_data = pb::RetryInfo {
            retry_delay: retry_delay,
        };

        Any {
            type_url: RetryInfo::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for RetryInfo {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let retry_info = pb::RetryInfo::decode(buf)?;

        let retry_delay = match retry_info.retry_delay {
            Some(duration) => {
                // Negative retry_delays become 0
                let duration = time::Duration::try_from(duration).unwrap_or(time::Duration::ZERO);
                Some(duration)
            }
            None => None,
        };

        let retry_info = RetryInfo {
            retry_delay: retry_delay,
        };

        Ok(retry_info)
    }
}

#[cfg(test)]
mod tests {

    use core::time::Duration;

    use super::super::super::{FromAny, IntoAny};
    use super::RetryInfo;

    #[test]
    fn gen_retry_info() {
        let error_info = RetryInfo::new(Some(Duration::from_secs(u64::MAX)));

        let formatted = format!("{:?}", error_info);

        println!("filled RetryInfo -> {formatted}");

        let expected_filled = "RetryInfo { retry_delay: Some(315576000000.999999999s) }";

        assert!(
            formatted.eq(expected_filled),
            "filled RetryInfo differs from expected result"
        );

        assert!(
            error_info.is_empty() == false,
            "filled RetryInfo returns 'false' from .has_retry_delay()"
        );

        let gen_any = error_info.into_any();

        let formatted = format!("{:?}", gen_any);

        println!("Any generated from RetryInfo -> {formatted}");

        let expected =
            "Any { type_url: \"type.googleapis.com/google.rpc.RetryInfo\", value: [10, 13, 8, 128, 188, 174, 206, 151, 9, 16, 255, 147, 235, 220, 3] }";

        assert!(
            formatted.eq(expected),
            "Any from filled RetryInfo differs from expected result"
        );

        let br_details = match RetryInfo::from_any(gen_any) {
            Err(error) => panic!("Error generating RetryInfo from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        println!("RetryInfo generated from Any -> {formatted}");

        assert!(
            formatted.eq(expected_filled),
            "RetryInfo from Any differs from expected result"
        );
    }
}
