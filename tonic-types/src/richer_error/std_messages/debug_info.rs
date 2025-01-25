use prost::{DecodeError, Message};
use prost_types::Any;

use crate::richer_error::FromAnyRef;

use super::super::{pb, FromAny, IntoAny};

/// Used to encode/decode the `DebugInfo` standard error message described in
/// [error_details.proto]. Describes additional debugging info.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct DebugInfo {
    /// Stack trace entries indicating where the error occurred.
    pub stack_entries: Vec<String>,

    /// Additional debugging information provided by the server.
    pub detail: String,
}

impl DebugInfo {
    /// Type URL of the `DebugInfo` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.DebugInfo";

    /// Creates a new [`DebugInfo`] struct.
    pub fn new(stack_entries: impl Into<Vec<String>>, detail: impl Into<String>) -> Self {
        DebugInfo {
            stack_entries: stack_entries.into(),
            detail: detail.into(),
        }
    }

    /// Returns `true` if [`DebugInfo`] fields are empty, and `false` if they
    /// are not.
    pub fn is_empty(&self) -> bool {
        self.stack_entries.is_empty() && self.detail.is_empty()
    }
}

impl IntoAny for DebugInfo {
    fn into_any(self) -> Any {
        let detail_data: pb::DebugInfo = self.into();

        Any {
            type_url: DebugInfo::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for DebugInfo {
    #[inline]
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        FromAnyRef::from_any_ref(&any)
    }
}

impl FromAnyRef for DebugInfo {
    fn from_any_ref(any: &Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let debug_info = pb::DebugInfo::decode(buf)?;

        Ok(debug_info.into())
    }
}

impl From<pb::DebugInfo> for DebugInfo {
    fn from(debug_info: pb::DebugInfo) -> Self {
        DebugInfo {
            stack_entries: debug_info.stack_entries,
            detail: debug_info.detail,
        }
    }
}

impl From<DebugInfo> for pb::DebugInfo {
    fn from(debug_info: DebugInfo) -> Self {
        pb::DebugInfo {
            stack_entries: debug_info.stack_entries,
            detail: debug_info.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{FromAny, IntoAny};
    use super::DebugInfo;

    #[test]
    fn gen_debug_info() {
        let debug_info = DebugInfo::new(
            vec!["trace 3".into(), "trace 2".into(), "trace 1".into()],
            "details about the error",
        );

        let formatted = format!("{:?}", debug_info);

        let expected_filled = "DebugInfo { stack_entries: [\"trace 3\", \"trace 2\", \"trace 1\"], detail: \"details about the error\" }";

        assert!(
            formatted.eq(expected_filled),
            "filled DebugInfo differs from expected result"
        );

        let gen_any = debug_info.into_any();
        let formatted = format!("{:?}", gen_any);

        let expected =
            "Any { type_url: \"type.googleapis.com/google.rpc.DebugInfo\", value: [10, 7, 116, 114, 97, 99, 101, 32, 51, 10, 7, 116, 114, 97, 99, 101, 32, 50, 10, 7, 116, 114, 97, 99, 101, 32, 49, 18, 23, 100, 101, 116, 97, 105, 108, 115, 32, 97, 98, 111, 117, 116, 32, 116, 104, 101, 32, 101, 114, 114, 111, 114] }";

        assert!(
            formatted.eq(expected),
            "Any from filled DebugInfo differs from expected result"
        );

        let br_details = match DebugInfo::from_any(gen_any) {
            Err(error) => panic!("Error generating DebugInfo from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        assert!(
            formatted.eq(expected_filled),
            "DebugInfo from Any differs from expected result"
        );
    }
}
