use prost::{DecodeError, Message};
use prost_types::Any;

use crate::richer_error::FromAnyRef;

use super::super::{pb, FromAny, IntoAny};

/// Used to encode/decode the `RequestInfo` standard error message described
/// in [error_details.proto]. Contains metadata about the request that
/// clients can attach when providing feedback.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct RequestInfo {
    /// An opaque string that should only be interpreted by the service that
    /// generated it. For example, an id used to identify requests in the logs.
    pub request_id: String,

    /// Any data used to serve this request. For example, an encrypted stack
    /// trace that can be sent back to the service provider for debugging.
    pub serving_data: String,
}

impl RequestInfo {
    /// Type URL of the `RequestInfo` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.RequestInfo";

    /// Creates a new [`RequestInfo`] struct.
    pub fn new(request_id: impl Into<String>, serving_data: impl Into<String>) -> Self {
        RequestInfo {
            request_id: request_id.into(),
            serving_data: serving_data.into(),
        }
    }

    /// Returns `true` if [`RequestInfo`] fields are empty, and `false` if they
    /// are not.
    pub fn is_empty(&self) -> bool {
        self.request_id.is_empty() && self.serving_data.is_empty()
    }
}

impl IntoAny for RequestInfo {
    fn into_any(self) -> Any {
        let detail_data: pb::RequestInfo = self.into();

        Any {
            type_url: RequestInfo::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for RequestInfo {
    #[inline]
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        FromAnyRef::from_any_ref(&any)
    }
}

impl FromAnyRef for RequestInfo {
    fn from_any_ref(any: &Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let req_info = pb::RequestInfo::decode(buf)?;

        Ok(req_info.into())
    }
}

impl From<pb::RequestInfo> for RequestInfo {
    fn from(req_info: pb::RequestInfo) -> Self {
        RequestInfo {
            request_id: req_info.request_id,
            serving_data: req_info.serving_data,
        }
    }
}

impl From<RequestInfo> for pb::RequestInfo {
    fn from(req_info: RequestInfo) -> Self {
        pb::RequestInfo {
            request_id: req_info.request_id,
            serving_data: req_info.serving_data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{FromAny, IntoAny};
    use super::RequestInfo;

    #[test]
    fn gen_request_info() {
        let req_info = RequestInfo::new("some-id", "some-data");

        let formatted = format!("{:?}", req_info);

        let expected_filled =
            "RequestInfo { request_id: \"some-id\", serving_data: \"some-data\" }";

        assert!(
            formatted.eq(expected_filled),
            "filled RequestInfo differs from expected result"
        );

        let gen_any = req_info.into_any();

        let formatted = format!("{:?}", gen_any);

        let expected =
            "Any { type_url: \"type.googleapis.com/google.rpc.RequestInfo\", value: [10, 7, 115, 111, 109, 101, 45, 105, 100, 18, 9, 115, 111, 109, 101, 45, 100, 97, 116, 97] }";

        assert!(
            formatted.eq(expected),
            "Any from filled RequestInfo differs from expected result"
        );

        let br_details = match RequestInfo::from_any(gen_any) {
            Err(error) => panic!("Error generating RequestInfo from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        assert!(
            formatted.eq(expected_filled),
            "RequestInfo from Any differs from expected result"
        );
    }
}
