use std::collections::HashMap;

use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::pb;
use super::super::{FromAny, IntoAny};

/// Used to encode/decode the `ErrorInfo` standard error message described in
/// [error_details.proto]. Describes the cause of the error with structured
/// details.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct ErrorInfo {
    /// Reason of the error. Should be a constant value that identifies the
    /// proximate cause of the error. Error reasons should be unique within a
    /// particular domain of errors. This should be at most 63 characters and
    /// match `/[A-Z0-9_]+/`.
    pub reason: String,

    /// Logical grouping to which the "reason" belongs. Normally is the
    /// registered name of the service that generates the error.
    pub domain: String,

    /// Additional structured details about this error. Keys should match
    /// `/[a-zA-Z0-9-_]/` and be limited to 64 characters in length.
    pub metadata: HashMap<String, String>,
}

impl ErrorInfo {
    /// Type URL of the `ErrorInfo` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.ErrorInfo";

    /// Creates a new [`ErrorInfo`] struct.
    pub fn new(
        reason: impl Into<String>,
        domain: impl Into<String>,
        metadata: HashMap<String, String>,
    ) -> Self {
        ErrorInfo {
            reason: reason.into(),
            domain: domain.into(),
            metadata: metadata,
        }
    }
}

impl ErrorInfo {
    /// Returns `true` if [`ErrorInfo`] fields are empty, and `false` if they
    /// are not.
    pub fn is_empty(&self) -> bool {
        self.reason.is_empty() && self.domain.is_empty() && self.metadata.is_empty()
    }
}

impl IntoAny for ErrorInfo {
    fn into_any(self) -> Any {
        let detail_data = pb::ErrorInfo {
            reason: self.reason,
            domain: self.domain,
            metadata: self.metadata,
        };

        Any {
            type_url: ErrorInfo::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for ErrorInfo {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let debug_info = pb::ErrorInfo::decode(buf)?;

        let debug_info = ErrorInfo {
            reason: debug_info.reason,
            domain: debug_info.domain,
            metadata: debug_info.metadata,
        };

        Ok(debug_info)
    }
}

#[cfg(test)]
mod tests {

    use std::collections::HashMap;

    use super::super::super::{FromAny, IntoAny};
    use super::ErrorInfo;

    #[test]
    fn gen_error_info() {
        let mut metadata = HashMap::new();
        metadata.insert("instanceLimitPerRequest".to_string(), "100".into());

        let error_info = ErrorInfo::new("SOME_INFO", "mydomain.com", metadata);

        let formatted = format!("{:?}", error_info);

        println!("filled ErrorInfo -> {formatted}");

        let expected_filled = "ErrorInfo { reason: \"SOME_INFO\", domain: \"mydomain.com\", metadata: {\"instanceLimitPerRequest\": \"100\"} }";

        assert!(
            formatted.eq(expected_filled),
            "filled ErrorInfo differs from expected result"
        );

        let gen_any = error_info.into_any();

        let formatted = format!("{:?}", gen_any);

        println!("Any generated from ErrorInfo -> {formatted}");

        let expected =
            "Any { type_url: \"type.googleapis.com/google.rpc.ErrorInfo\", value: [10, 9, 83, 79, 77, 69, 95, 73, 78, 70, 79, 18, 12, 109, 121, 100, 111, 109, 97, 105, 110, 46, 99, 111, 109, 26, 30, 10, 23, 105, 110, 115, 116, 97, 110, 99, 101, 76, 105, 109, 105, 116, 80, 101, 114, 82, 101, 113, 117, 101, 115, 116, 18, 3, 49, 48, 48] }";

        assert!(
            formatted.eq(expected),
            "Any from filled ErrorInfo differs from expected result"
        );

        let br_details = match ErrorInfo::from_any(gen_any) {
            Err(error) => panic!("Error generating ErrorInfo from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        println!("ErrorInfo generated from Any -> {formatted}");

        assert!(
            formatted.eq(expected_filled),
            "ErrorInfo from Any differs from expected result"
        );
    }
}
