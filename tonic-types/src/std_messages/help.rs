use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::pb;
use super::super::{FromAny, IntoAny};

/// Used at the `links` field of the [`Help`] struct. Describes a URL link.
#[derive(Clone, Debug)]
pub struct HelpLink {
    /// Description of what the link offers.
    pub description: String,

    /// URL of the link.
    pub url: String,
}

impl HelpLink {
    /// Creates a new [`HelpLink`] struct.
    pub fn new(description: impl Into<String>, url: impl Into<String>) -> Self {
        HelpLink {
            description: description.into(),
            url: url.into(),
        }
    }
}

/// Used to encode/decode the `Help` standard error message described in
/// [error_details.proto]. Provides links to documentation or for performing
/// an out-of-band action.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct Help {
    /// Links pointing to additional information on how to handle the error.
    pub links: Vec<HelpLink>,
}

impl Help {
    /// Type URL of the `Help` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.Help";

    /// Creates a new [`Help`] struct.
    pub fn new(links: Vec<HelpLink>) -> Self {
        Help { links }
    }

    /// Creates a new [`Help`] struct with a single [`HelpLink`] in `links`.
    pub fn with_link(description: impl Into<String>, url: impl Into<String>) -> Self {
        Help {
            links: vec![HelpLink {
                description: description.into(),
                url: url.into(),
            }],
        }
    }
}

impl Help {
    /// Adds a [`HelpLink`] to [`Help`]'s `links` vector.
    pub fn add_link(
        &mut self,
        description: impl Into<String>,
        url: impl Into<String>,
    ) -> &mut Self {
        self.links.append(&mut vec![HelpLink {
            description: description.into(),
            url: url.into(),
        }]);
        self
    }

    /// Returns `true` if [`Help`]'s `links` vector is empty, and `false` if it
    /// is not.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

impl IntoAny for Help {
    fn into_any(self) -> Any {
        let detail_data = pb::Help {
            links: self
                .links
                .into_iter()
                .map(|v| pb::help::Link {
                    description: v.description,
                    url: v.url,
                })
                .collect(),
        };

        Any {
            type_url: Help::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for Help {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let help = pb::Help::decode(buf)?;

        let quota_failure = Help {
            links: help
                .links
                .into_iter()
                .map(|v| HelpLink {
                    description: v.description,
                    url: v.url,
                })
                .collect(),
        };

        Ok(quota_failure)
    }
}

#[cfg(test)]
mod tests {

    use super::super::super::{FromAny, IntoAny};
    use super::Help;

    #[test]
    fn gen_quota_failure() {
        let mut help = Help::new(Vec::new());
        let formatted = format!("{:?}", help);

        println!("empty Help -> {formatted}");

        let expected = "Help { links: [] }";

        assert!(
            formatted.eq(expected),
            "empty Help differs from expected result"
        );

        assert!(
            help.is_empty(),
            "empty Help returns 'false' from .is_empty()"
        );

        help.add_link("link to resource a", "resource-a.example.local")
            .add_link("link to resource b", "resource-b.example.local");

        let formatted = format!("{:?}", help);

        println!("filled Help -> {formatted}");

        let expected_filled = "Help { links: [HelpLink { description: \"link to resource a\", url: \"resource-a.example.local\" }, HelpLink { description: \"link to resource b\", url: \"resource-b.example.local\" }] }";

        assert!(
            formatted.eq(expected_filled),
            "filled Help differs from expected result"
        );

        assert!(
            help.is_empty() == false,
            "filled Help returns 'true' from .is_empty()"
        );

        let gen_any = help.into_any();

        let formatted = format!("{:?}", gen_any);

        println!("Any generated from Help -> {formatted}");

        let expected = "Any { type_url: \"type.googleapis.com/google.rpc.Help\", value: [10, 46, 10, 18, 108, 105, 110, 107, 32, 116, 111, 32, 114, 101, 115, 111, 117, 114, 99, 101, 32, 97, 18, 24, 114, 101, 115, 111, 117, 114, 99, 101, 45, 97, 46, 101, 120, 97, 109, 112, 108, 101, 46, 108, 111, 99, 97, 108, 10, 46, 10, 18, 108, 105, 110, 107, 32, 116, 111, 32, 114, 101, 115, 111, 117, 114, 99, 101, 32, 98, 18, 24, 114, 101, 115, 111, 117, 114, 99, 101, 45, 98, 46, 101, 120, 97, 109, 112, 108, 101, 46, 108, 111, 99, 97, 108] }";

        assert!(
            formatted.eq(expected),
            "Any from filled Help differs from expected result"
        );

        let br_details = match Help::from_any(gen_any) {
            Err(error) => panic!("Error generating Help from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        println!("Help generated from Any -> {formatted}");

        assert!(
            formatted.eq(expected_filled),
            "Help from Any differs from expected result"
        );
    }
}
