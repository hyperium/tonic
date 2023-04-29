use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::{pb, FromAny, IntoAny};

/// Used to encode/decode the `LocalizedMessage` standard error message
/// described in [error_details.proto]. Provides a localized error message
/// that is safe to return to the user.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct LocalizedMessage {
    /// Locale used, following the specification defined in [BCP 47]. For
    /// example: "en-US", "fr-CH" or "es-MX".
    ///
    /// [BCP 47]: http://www.rfc-editor.org/rfc/bcp/bcp47.txt
    pub locale: String,

    /// Message corresponding to the locale.
    pub message: String,
}

impl LocalizedMessage {
    /// Type URL of the `LocalizedMessage` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.LocalizedMessage";

    /// Creates a new [`LocalizedMessage`] struct.
    pub fn new(locale: impl Into<String>, message: impl Into<String>) -> Self {
        LocalizedMessage {
            locale: locale.into(),
            message: message.into(),
        }
    }

    /// Returns `true` if [`LocalizedMessage`] fields are empty, and `false` if
    /// they are not.
    pub fn is_empty(&self) -> bool {
        self.locale.is_empty() && self.message.is_empty()
    }
}

impl IntoAny for LocalizedMessage {
    fn into_any(self) -> Any {
        let detail_data = pb::LocalizedMessage {
            locale: self.locale,
            message: self.message,
        };

        Any {
            type_url: LocalizedMessage::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for LocalizedMessage {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let loc_message = pb::LocalizedMessage::decode(buf)?;

        let loc_message = LocalizedMessage {
            locale: loc_message.locale,
            message: loc_message.message,
        };

        Ok(loc_message)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{FromAny, IntoAny};
    use super::LocalizedMessage;

    #[test]
    fn gen_localized_message() {
        let loc_message = LocalizedMessage::new("en-US", "message for the user");

        let formatted = format!("{:?}", loc_message);

        let expected_filled =
            "LocalizedMessage { locale: \"en-US\", message: \"message for the user\" }";

        assert!(
            formatted.eq(expected_filled),
            "filled LocalizedMessage differs from expected result"
        );

        let gen_any = loc_message.into_any();

        let formatted = format!("{:?}", gen_any);

        let expected =
            "Any { type_url: \"type.googleapis.com/google.rpc.LocalizedMessage\", value: [10, 5, 101, 110, 45, 85, 83, 18, 20, 109, 101, 115, 115, 97, 103, 101, 32, 102, 111, 114, 32, 116, 104, 101, 32, 117, 115, 101, 114] }";

        assert!(
            formatted.eq(expected),
            "Any from filled LocalizedMessage differs from expected result"
        );

        let br_details = match LocalizedMessage::from_any(gen_any) {
            Err(error) => panic!("Error generating LocalizedMessage from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        assert!(
            formatted.eq(expected_filled),
            "LocalizedMessage from Any differs from expected result"
        );
    }
}
