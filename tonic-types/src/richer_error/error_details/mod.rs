use std::time;

use super::std_messages::{BadRequest, FieldViolation, RetryInfo};

pub(crate) mod vec;

/// Groups the standard error messages structs. Provides associated
/// functions and methods to setup and edit each error message independently.
/// Used when extracting error details from `tonic::Status`, and when
/// creating a `tonic::Status` with error details.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct ErrorDetails {
    /// This field stores [`RetryInfo`] data, if any.
    pub(crate) retry_info: Option<RetryInfo>,

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
        ErrorDetails {
            retry_info: None,
            bad_request: None,
        }
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

impl Default for ErrorDetails {
    fn default() -> Self {
        Self::new()
    }
}
