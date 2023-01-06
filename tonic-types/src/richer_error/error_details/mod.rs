use std::time;

use super::std_messages::{
    BadRequest, DebugInfo, FieldViolation, QuotaFailure, QuotaViolation, RetryInfo,
};

pub(crate) mod vec;

/// Groups the standard error messages structs. Provides associated
/// functions and methods to setup and edit each error message independently.
/// Used when extracting error details from `tonic::Status`, and when
/// creating a `tonic::Status` with error details.
#[non_exhaustive]
#[derive(Clone, Debug, Default)]
pub struct ErrorDetails {
    /// This field stores [`RetryInfo`] data, if any.
    pub(crate) retry_info: Option<RetryInfo>,

    /// This field stores [`DebugInfo`] data, if any.
    pub(crate) debug_info: Option<DebugInfo>,

    /// This field stores [`QuotaFailure`] data, if any.
    pub(crate) quota_failure: Option<QuotaFailure>,

    /// This field stores [`BadRequest`] data, if any.
    pub(crate) bad_request: Option<BadRequest>,
}

impl ErrorDetails {
    /// Generates an [`ErrorDetails`] struct with all fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Generates an [`ErrorDetails`] struct with [`RetryInfo`] details and
    /// remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use tonic_types::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_retry_info(Some(Duration::from_secs(5)));
    /// ```
    pub fn with_retry_info(retry_delay: Option<time::Duration>) -> Self {
        ErrorDetails {
            retry_info: Some(RetryInfo::new(retry_delay)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`DebugInfo`] details and
    /// remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let err_stack = vec!["...".into(), "...".into()];
    ///
    /// let err_details = ErrorDetails::with_debug_info(err_stack, "error details");
    /// ```
    pub fn with_debug_info(stack_entries: Vec<String>, detail: impl Into<String>) -> Self {
        ErrorDetails {
            debug_info: Some(DebugInfo::new(stack_entries, detail)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`QuotaFailure`] details and
    /// remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails, QuotaViolation};
    ///
    /// let err_details = ErrorDetails::with_quota_failure(vec![
    ///     QuotaViolation::new("subject 1", "description 1"),
    ///     QuotaViolation::new("subject 2", "description 2"),
    /// ]);
    /// ```
    pub fn with_quota_failure(violations: Vec<QuotaViolation>) -> Self {
        ErrorDetails {
            quota_failure: Some(QuotaFailure::new(violations)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`QuotaFailure`] details (one
    /// [`QuotaViolation`] set) and remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_quota_failure_violation("subject", "description");
    /// ```
    pub fn with_quota_failure_violation(
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ErrorDetails {
            quota_failure: Some(QuotaFailure::with_violation(subject, description)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`BadRequest`] details and
    /// remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails, FieldViolation};
    ///
    /// let err_details = ErrorDetails::with_bad_request(vec![
    ///     FieldViolation::new("field_1", "description 1"),
    ///     FieldViolation::new("field_2", "description 2"),
    /// ]);
    /// ```
    pub fn with_bad_request(field_violations: Vec<FieldViolation>) -> Self {
        ErrorDetails {
            bad_request: Some(BadRequest::new(field_violations)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`BadRequest`] details (one
    /// [`FieldViolation`] set) and remaining fields set to `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_bad_request_violation(
    ///     "field",
    ///     "description",
    /// );
    /// ```
    pub fn with_bad_request_violation(
        field: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ErrorDetails {
            bad_request: Some(BadRequest::with_violation(field, description)),
            ..ErrorDetails::new()
        }
    }

    /// Get [`RetryInfo`] details, if any
    pub fn retry_info(&self) -> Option<RetryInfo> {
        self.retry_info.clone()
    }

    /// Get [`DebugInfo`] details, if any
    pub fn debug_info(&self) -> Option<DebugInfo> {
        self.debug_info.clone()
    }

    /// Get [`QuotaFailure`] details, if any
    pub fn quota_failure(&self) -> Option<QuotaFailure> {
        self.quota_failure.clone()
    }

    /// Get [`BadRequest`] details, if any
    pub fn bad_request(&self) -> Option<BadRequest> {
        self.bad_request.clone()
    }

    /// Set [`RetryInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_retry_info(Some(Duration::from_secs(5)));
    /// ```
    pub fn set_retry_info(&mut self, retry_delay: Option<time::Duration>) -> &mut Self {
        self.retry_info = Some(RetryInfo::new(retry_delay));
        self
    }

    /// Set [`DebugInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// let err_stack = vec!["...".into(), "...".into()];
    ///
    /// err_details.set_debug_info(err_stack, "error details");
    /// ```
    pub fn set_debug_info(
        &mut self,
        stack_entries: Vec<String>,
        detail: impl Into<String>,
    ) -> &mut Self {
        self.debug_info = Some(DebugInfo::new(stack_entries, detail));
        self
    }

    /// Set [`QuotaFailure`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails, QuotaViolation};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_quota_failure(vec![
    ///     QuotaViolation::new("subject 1", "description 1"),
    ///     QuotaViolation::new("subject 2", "description 2"),
    /// ]);
    /// ```
    pub fn set_quota_failure(&mut self, violations: Vec<QuotaViolation>) -> &mut Self {
        self.quota_failure = Some(QuotaFailure::new(violations));
        self
    }

    /// Adds a [`QuotaViolation`] to [`QuotaFailure`] details. Sets
    /// [`QuotaFailure`] details if it is not set yet. Can be chained with
    /// other `.set_` and `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.add_quota_failure_violation("subject", "description");
    /// ```
    pub fn add_quota_failure_violation(
        &mut self,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        match &mut self.quota_failure {
            Some(quota_failure) => {
                quota_failure.add_violation(subject, description);
            }
            None => {
                self.quota_failure = Some(QuotaFailure::with_violation(subject, description));
            }
        };
        self
    }

    /// Returns `true` if [`QuotaFailure`] is set and its `violations` vector
    /// is not empty, otherwise returns `false`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::with_quota_failure(vec![]);
    ///
    /// assert_eq!(err_details.has_quota_failure_violations(), false);
    ///
    /// err_details.add_quota_failure_violation("subject", "description");
    ///
    /// assert_eq!(err_details.has_quota_failure_violations(), true);
    /// ```
    pub fn has_quota_failure_violations(&self) -> bool {
        if let Some(quota_failure) = &self.quota_failure {
            return !quota_failure.violations.is_empty();
        }
        false
    }

    /// Set [`BadRequest`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails, FieldViolation};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_bad_request(vec![
    ///     FieldViolation::new("field_1", "description 1"),
    ///     FieldViolation::new("field_2", "description 2"),
    /// ]);
    /// ```
    pub fn set_bad_request(&mut self, violations: Vec<FieldViolation>) -> &mut Self {
        self.bad_request = Some(BadRequest::new(violations));
        self
    }

    /// Adds a [`FieldViolation`] to [`BadRequest`] details. Sets
    /// [`BadRequest`] details if it is not set yet. Can be chained with other
    /// `.set_` and `.add_` [`ErrorDetails`] methods.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.add_bad_request_violation("field", "description");
    /// ```
    pub fn add_bad_request_violation(
        &mut self,
        field: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        match &mut self.bad_request {
            Some(bad_request) => {
                bad_request.add_violation(field, description);
            }
            None => {
                self.bad_request = Some(BadRequest::with_violation(field, description));
            }
        };
        self
    }

    /// Returns `true` if [`BadRequest`] is set and its `field_violations`
    /// vector is not empty, otherwise returns `false`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic_types::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::with_bad_request(vec![]);
    ///
    /// assert_eq!(err_details.has_bad_request_violations(), false);
    ///
    /// err_details.add_bad_request_violation("field", "description");
    ///
    /// assert_eq!(err_details.has_bad_request_violations(), true);
    /// ```
    pub fn has_bad_request_violations(&self) -> bool {
        if let Some(bad_request) = &self.bad_request {
            return !bad_request.field_violations.is_empty();
        }
        false
    }
}
