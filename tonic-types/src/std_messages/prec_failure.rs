use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::pb;
use super::super::{FromAny, IntoAny};

/// Used at the `violations` field of the [`PreconditionFailure`] struct.
/// Describes a single precondition failure.
#[derive(Clone, Debug)]
pub struct PreconditionViolation {
    /// Type of the PreconditionFailure. At [error_details.proto], the usage
    /// of a service-specific enum type is recommended. For example, "TOS" for
    /// a "Terms of Service" violation.
    ///
    /// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
    pub r#type: String,

    /// Subject, relative to the type, that failed.
    pub subject: String,

    /// A description of how the precondition failed.
    pub description: String,
}

impl PreconditionViolation {
    /// Creates a new [`PreconditionViolation`] struct.
    pub fn new(
        r#type: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        PreconditionViolation {
            r#type: r#type.into(),
            subject: subject.into(),
            description: description.into(),
        }
    }
}

/// Used to encode/decode the `PreconditionFailure` standard error message
/// described in [error_details.proto]. Describes what preconditions have
/// failed.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct PreconditionFailure {
    /// Describes all precondition violations of the request.
    pub violations: Vec<PreconditionViolation>,
}

impl PreconditionFailure {
    /// Type URL of the `PreconditionFailure` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.PreconditionFailure";

    /// Creates a new [`PreconditionFailure`] struct.
    pub fn new(violations: Vec<PreconditionViolation>) -> Self {
        PreconditionFailure {
            violations: violations,
        }
    }

    /// Creates a new [`PreconditionFailure`] struct with a single
    /// [`PreconditionViolation`] in `violations`.
    pub fn with_violation(
        violation_type: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        PreconditionFailure {
            violations: vec![PreconditionViolation {
                r#type: violation_type.into(),
                subject: subject.into(),
                description: description.into(),
            }],
        }
    }
}

impl PreconditionFailure {
    /// Adds a [`PreconditionViolation`] to [`PreconditionFailure`]'s
    /// `violations` vector.
    pub fn add_violation(
        &mut self,
        r#type: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        self.violations.append(&mut vec![PreconditionViolation {
            r#type: r#type.into(),
            subject: subject.into(),
            description: description.into(),
        }]);
        self
    }

    /// Returns `true` if [`PreconditionFailure`]'s `violations` vector is
    /// empty, and `false` if it is not.
    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }
}

impl IntoAny for PreconditionFailure {
    fn into_any(self) -> Any {
        let detail_data = pb::PreconditionFailure {
            violations: self
                .violations
                .into_iter()
                .map(|v| pb::precondition_failure::Violation {
                    r#type: v.r#type,
                    subject: v.subject,
                    description: v.description,
                })
                .collect(),
        };

        Any {
            type_url: PreconditionFailure::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for PreconditionFailure {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let precondition_failure = pb::PreconditionFailure::decode(buf)?;

        let precondition_failure = PreconditionFailure {
            violations: precondition_failure
                .violations
                .into_iter()
                .map(|v| PreconditionViolation {
                    r#type: v.r#type,
                    subject: v.subject,
                    description: v.description,
                })
                .collect(),
        };

        Ok(precondition_failure)
    }
}

#[cfg(test)]
mod tests {

    use super::super::super::{FromAny, IntoAny};
    use super::PreconditionFailure;

    #[test]
    fn gen_prec_failure() {
        let mut prec_failure = PreconditionFailure::new(Vec::new());
        let formatted = format!("{:?}", prec_failure);

        println!("empty PreconditionFailure -> {formatted}");

        let expected = "PreconditionFailure { violations: [] }";

        assert!(
            formatted.eq(expected),
            "empty PreconditionFailure differs from expected result"
        );

        assert!(
            prec_failure.is_empty(),
            "empty PreconditionFailure returns 'false' from .is_empty()"
        );

        prec_failure
            .add_violation("TOS", "example.local", "Terms of service not accepted")
            .add_violation("FNF", "example.local", "File not found");

        let formatted = format!("{:?}", prec_failure);

        println!("filled PreconditionFailure -> {formatted}");

        let expected_filled = "PreconditionFailure { violations: [PreconditionViolation { type: \"TOS\", subject: \"example.local\", description: \"Terms of service not accepted\" }, PreconditionViolation { type: \"FNF\", subject: \"example.local\", description: \"File not found\" }] }";

        assert!(
            formatted.eq(expected_filled),
            "filled PreconditionFailure differs from expected result"
        );

        assert!(
            prec_failure.is_empty() == false,
            "filled PreconditionFailure returns 'true' from .is_empty()"
        );

        let gen_any = prec_failure.into_any();

        let formatted = format!("{:?}", gen_any);

        println!("Any generated from PreconditionFailure -> {formatted}");

        let expected = "Any { type_url: \"type.googleapis.com/google.rpc.PreconditionFailure\", value: [10, 51, 10, 3, 84, 79, 83, 18, 13, 101, 120, 97, 109, 112, 108, 101, 46, 108, 111, 99, 97, 108, 26, 29, 84, 101, 114, 109, 115, 32, 111, 102, 32, 115, 101, 114, 118, 105, 99, 101, 32, 110, 111, 116, 32, 97, 99, 99, 101, 112, 116, 101, 100, 10, 36, 10, 3, 70, 78, 70, 18, 13, 101, 120, 97, 109, 112, 108, 101, 46, 108, 111, 99, 97, 108, 26, 14, 70, 105, 108, 101, 32, 110, 111, 116, 32, 102, 111, 117, 110, 100] }";

        assert!(
            formatted.eq(expected),
            "Any from filled PreconditionFailure differs from expected result"
        );

        let br_details = match PreconditionFailure::from_any(gen_any) {
            Err(error) => panic!("Error generating PreconditionFailure from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        println!("PreconditionFailure generated from Any -> {formatted}");

        assert!(
            formatted.eq(expected_filled),
            "PreconditionFailure from Any differs from expected result"
        );
    }
}
