use prost::{DecodeError, Message};
use prost_types::Any;

use super::super::pb;
use super::super::{FromAny, IntoAny};

/// Used at the `violations` field of the [`QuotaFailure`] struct. Describes a
/// single quota violation.
#[derive(Clone, Debug)]
pub struct QuotaViolation {
    /// Subject on which the quota check failed.
    pub subject: String,

    /// Description of why the quota check failed.
    pub description: String,
}

impl QuotaViolation {
    /// Creates a new [`QuotaViolation`] struct.
    pub fn new(subject: impl Into<String>, description: impl Into<String>) -> Self {
        QuotaViolation {
            subject: subject.into(),
            description: description.into(),
        }
    }
}

/// Used to encode/decode the `QuotaFailure` standard error message described
/// in [error_details.proto]. Describes how a quota check failed.
///
/// [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
#[derive(Clone, Debug)]
pub struct QuotaFailure {
    /// Describes all quota violations.
    pub violations: Vec<QuotaViolation>,
}

impl QuotaFailure {
    /// Type URL of the `QuotaFailure` standard error message type.
    pub const TYPE_URL: &'static str = "type.googleapis.com/google.rpc.QuotaFailure";

    /// Creates a new [`QuotaFailure`] struct.
    pub fn new(violations: Vec<QuotaViolation>) -> Self {
        QuotaFailure {
            violations: violations,
        }
    }

    /// Creates a new [`QuotaFailure`] struct with a single [`QuotaViolation`]
    /// in `violations`.
    pub fn with_violation(subject: impl Into<String>, description: impl Into<String>) -> Self {
        QuotaFailure {
            violations: vec![QuotaViolation {
                subject: subject.into(),
                description: description.into(),
            }],
        }
    }
}

impl QuotaFailure {
    /// Adds a [`QuotaViolation`] to [`QuotaFailure`]'s `violations`.
    pub fn add_violation(
        &mut self,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        self.violations.append(&mut vec![QuotaViolation {
            subject: subject.into(),
            description: description.into(),
        }]);
        self
    }

    /// Returns `true` if [`QuotaFailure`]'s `violations` vector is empty, and
    /// `false` if it is not.
    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }
}

impl IntoAny for QuotaFailure {
    fn into_any(self) -> Any {
        let detail_data = pb::QuotaFailure {
            violations: self
                .violations
                .into_iter()
                .map(|v| pb::quota_failure::Violation {
                    subject: v.subject,
                    description: v.description,
                })
                .collect(),
        };

        Any {
            type_url: QuotaFailure::TYPE_URL.to_string(),
            value: detail_data.encode_to_vec(),
        }
    }
}

impl FromAny for QuotaFailure {
    fn from_any(any: Any) -> Result<Self, DecodeError> {
        let buf: &[u8] = &any.value;
        let quota_failure = pb::QuotaFailure::decode(buf)?;

        let quota_failure = QuotaFailure {
            violations: quota_failure
                .violations
                .into_iter()
                .map(|v| QuotaViolation {
                    subject: v.subject,
                    description: v.description,
                })
                .collect(),
        };

        Ok(quota_failure)
    }
}

#[cfg(test)]
mod tests {

    use super::super::super::{FromAny, IntoAny};
    use super::QuotaFailure;

    #[test]
    fn gen_quota_failure() {
        let mut quota_failure = QuotaFailure::new(Vec::new());
        let formatted = format!("{:?}", quota_failure);

        println!("empty QuotaFailure -> {formatted}");

        let expected = "QuotaFailure { violations: [] }";

        assert!(
            formatted.eq(expected),
            "empty QuotaFailure differs from expected result"
        );

        assert!(
            quota_failure.is_empty(),
            "empty QuotaFailure returns 'false' from .is_empty()"
        );

        quota_failure
            .add_violation("clientip:<ip address>", "description a")
            .add_violation("project:<project id>", "description b");

        let formatted = format!("{:?}", quota_failure);

        println!("filled QuotaFailure -> {formatted}");

        let expected_filled = "QuotaFailure { violations: [QuotaViolation { subject: \"clientip:<ip address>\", description: \"description a\" }, QuotaViolation { subject: \"project:<project id>\", description: \"description b\" }] }";

        assert!(
            formatted.eq(expected_filled),
            "filled QuotaFailure differs from expected result"
        );

        assert!(
            quota_failure.is_empty() == false,
            "filled QuotaFailure returns 'true' from .is_empty()"
        );

        let gen_any = quota_failure.into_any();

        let formatted = format!("{:?}", gen_any);

        println!("Any generated from QuotaFailure -> {formatted}");

        let expected = "Any { type_url: \"type.googleapis.com/google.rpc.QuotaFailure\", value: [10, 38, 10, 21, 99, 108, 105, 101, 110, 116, 105, 112, 58, 60, 105, 112, 32, 97, 100, 100, 114, 101, 115, 115, 62, 18, 13, 100, 101, 115, 99, 114, 105, 112, 116, 105, 111, 110, 32, 97, 10, 37, 10, 20, 112, 114, 111, 106, 101, 99, 116, 58, 60, 112, 114, 111, 106, 101, 99, 116, 32, 105, 100, 62, 18, 13, 100, 101, 115, 99, 114, 105, 112, 116, 105, 111, 110, 32, 98] }";

        assert!(
            formatted.eq(expected),
            "Any from filled QuotaFailure differs from expected result"
        );

        let br_details = match QuotaFailure::from_any(gen_any) {
            Err(error) => panic!("Error generating QuotaFailure from Any: {:?}", error),
            Ok(from_any) => from_any,
        };

        let formatted = format!("{:?}", br_details);

        println!("QuotaFailure generated from Any -> {formatted}");

        assert!(
            formatted.eq(expected_filled),
            "QuotaFailure from Any differs from expected result"
        );
    }
}
