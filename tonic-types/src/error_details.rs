use std::{collections::HashMap, time};

use super::std_messages::*;

/// Groups the standard error messages structs. Provides associated
/// functions and methods to setup and edit each error message independently.
/// Used when extracting error details from `tonic::Status`, and when
/// creating a `tonic::Status` with error details.
#[derive(Clone, Debug)]
pub struct ErrorDetails {
    /// This field stores [`RetryInfo`] data, if any.
    pub retry_info: Option<RetryInfo>,

    /// This field stores [`DebugInfo`] data, if any.
    pub debug_info: Option<DebugInfo>,

    /// This field stores [`QuotaFailure`] data, if any.
    pub quota_failure: Option<QuotaFailure>,

    /// This field stores [`ErrorInfo`] data, if any.
    pub error_info: Option<ErrorInfo>,

    /// This field stores [`PreconditionFailure`] data, if any.
    pub precondition_failure: Option<PreconditionFailure>,

    /// This field stores [`BadRequest`] data, if any.
    pub bad_request: Option<BadRequest>,

    /// This field stores [`RequestInfo`] data, if any.
    pub request_info: Option<RequestInfo>,

    /// This field stores [`ResourceInfo`] data, if any.
    pub resource_info: Option<ResourceInfo>,

    /// This field stores [`Help`] data, if any.
    pub help: Option<Help>,

    /// This field stores [`LocalizedMessage`] data, if any.
    pub localized_message: Option<LocalizedMessage>,
}

impl ErrorDetails {
    /// Generates an [`ErrorDetails`] struct with all fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::new();
    /// ```
    pub fn new() -> Self {
        ErrorDetails {
            retry_info: None,
            debug_info: None,
            quota_failure: None,
            error_info: None,
            precondition_failure: None,
            bad_request: None,
            request_info: None,
            resource_info: None,
            help: None,
            localized_message: None,
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`RetryInfo`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, QuotaViolation};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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

    /// Generates an [`ErrorDetails`] struct with [`ErrorInfo`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut metadata: HashMap<String, String> = HashMap::new();
    /// metadata.insert("instanceLimitPerRequest".into(), "100".into());
    ///
    /// let err_details = ErrorDetails::with_error_info("reason", "domain", metadata);
    /// ```
    pub fn with_error_info(
        reason: impl Into<String>,
        domain: impl Into<String>,
        metadata: HashMap<String, String>,
    ) -> Self {
        ErrorDetails {
            error_info: Some(ErrorInfo::new(reason, domain, metadata)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`PreconditionFailure`]
    /// details and remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, PreconditionViolation};
    ///
    /// let err_details = ErrorDetails::with_precondition_failure(vec![
    ///     PreconditionViolation::new(
    ///         "violation type 1",
    ///         "subject 1",
    ///         "description 1",
    ///     ),
    ///     PreconditionViolation::new(
    ///         "violation type 2",
    ///         "subject 2",
    ///         "description 2",
    ///     ),
    /// ]);
    /// ```
    pub fn with_precondition_failure(violations: Vec<PreconditionViolation>) -> Self {
        ErrorDetails {
            precondition_failure: Some(PreconditionFailure::new(violations)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`PreconditionFailure`]
    /// details (one [`PreconditionViolation`] set) and remaining fields set to
    /// `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_precondition_failure_violation(
    ///     "violation type",
    ///     "subject",
    ///     "description",
    /// );
    /// ```
    pub fn with_precondition_failure_violation(
        violation_type: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ErrorDetails {
            precondition_failure: Some(PreconditionFailure::with_violation(
                violation_type,
                subject,
                description,
            )),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`BadRequest`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, FieldViolation};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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

    /// Generates an [`ErrorDetails`] struct with [`RequestInfo`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_request_info(
    ///     "request_id",
    ///     "serving_data",
    /// );
    /// ```
    pub fn with_request_info(
        request_id: impl Into<String>,
        serving_data: impl Into<String>,
    ) -> Self {
        ErrorDetails {
            request_info: Some(RequestInfo::new(request_id, serving_data)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`ResourceInfo`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_resource_info(
    ///     "res_type",
    ///     "res_name",
    ///     "owner",
    ///     "description",
    /// );
    /// ```
    pub fn with_resource_info(
        resource_type: impl Into<String>,
        resource_name: impl Into<String>,
        owner: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ErrorDetails {
            resource_info: Some(ResourceInfo::new(
                resource_type,
                resource_name,
                owner,
                description,
            )),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`Help`] details and
    /// remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, HelpLink};
    ///
    /// let err_details = ErrorDetails::with_help(vec![
    ///     HelpLink::new("description of link a", "resource-a.example.local"),
    ///     HelpLink::new("description of link b", "resource-b.example.local"),
    /// ]);
    /// ```
    pub fn with_help(links: Vec<HelpLink>) -> Self {
        ErrorDetails {
            help: Some(Help::new(links)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`Help`] details (one
    /// [`HelpLink`] set) and remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_help_link(
    ///     "description of link a",
    ///     "resource-a.example.local"
    /// );
    /// ```
    pub fn with_help_link(description: impl Into<String>, url: impl Into<String>) -> Self {
        ErrorDetails {
            help: Some(Help::with_link(description, url)),
            ..ErrorDetails::new()
        }
    }

    /// Generates an [`ErrorDetails`] struct with [`LocalizedMessage`] details
    /// and remaining fields set to `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let err_details = ErrorDetails::with_localized_message(
    ///     "en-US",
    ///     "message for the user"
    /// );
    /// ```
    pub fn with_localized_message(locale: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorDetails {
            localized_message: Some(LocalizedMessage::new(locale, message)),
            ..ErrorDetails::new()
        }
    }
}

impl ErrorDetails {
    /// Set [`RetryInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, QuotaViolation};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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

    /// Set [`ErrorInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// let mut metadata: HashMap<String, String> = HashMap::new();
    /// metadata.insert("instanceLimitPerRequest".into(), "100".into());
    ///
    /// err_details.set_error_info("reason", "example.local", metadata);
    /// ```
    pub fn set_error_info(
        &mut self,
        reason: impl Into<String>,
        domain: impl Into<String>,
        metadata: HashMap<String, String>,
    ) -> &mut Self {
        self.error_info = Some(ErrorInfo::new(reason, domain, metadata));
        self
    }

    /// Set [`PreconditionFailure`] details. Can be chained with other `.set_`
    /// and `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, PreconditionViolation};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_precondition_failure(vec![
    ///     PreconditionViolation::new(
    ///         "violation type 1",
    ///         "subject 1",
    ///         "description 1",
    ///     ),
    ///     PreconditionViolation::new(
    ///         "violation type 2",
    ///         "subject 2",
    ///         "description 2",
    ///     ),
    /// ]);
    /// ```
    pub fn set_precondition_failure(
        &mut self,
        violations: Vec<PreconditionViolation>,
    ) -> &mut Self {
        self.precondition_failure = Some(PreconditionFailure::new(violations));
        self
    }

    /// Adds a [`PreconditionViolation`] to [`PreconditionFailure`] details.
    /// Sets [`PreconditionFailure`] details if it is not set yet. Can be
    /// chained with other `.set_` and `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.add_precondition_failure_violation(
    ///     "violation type",
    ///     "subject",
    ///     "description"
    /// );
    /// ```
    pub fn add_precondition_failure_violation(
        &mut self,
        violation_type: impl Into<String>,
        subject: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        match &mut self.precondition_failure {
            Some(precondition_failure) => {
                precondition_failure.add_violation(violation_type, subject, description);
            }
            None => {
                self.precondition_failure = Some(PreconditionFailure::with_violation(
                    violation_type,
                    subject,
                    description,
                ));
            }
        };
        self
    }

    /// Returns `true` if [`PreconditionFailure`] is set and its `violations`
    /// vector is not empty, otherwise returns `false`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::with_precondition_failure(vec![]);
    ///
    /// assert_eq!(err_details.has_precondition_failure_violations(), false);
    ///
    /// err_details.add_precondition_failure_violation(
    ///     "violation type",
    ///     "subject",
    ///     "description"
    /// );
    ///
    /// assert_eq!(err_details.has_precondition_failure_violations(), true);
    /// ```
    pub fn has_precondition_failure_violations(&self) -> bool {
        if let Some(precondition_failure) = &self.precondition_failure {
            return !precondition_failure.violations.is_empty();
        }
        false
    }

    /// Set [`BadRequest`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, FieldViolation};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
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

    /// Set [`RequestInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_request_info("request_id", "serving_data");
    /// ```
    pub fn set_request_info(
        &mut self,
        request_id: impl Into<String>,
        serving_data: impl Into<String>,
    ) -> &mut Self {
        self.request_info = Some(RequestInfo::new(request_id, serving_data));
        self
    }

    /// Set [`ResourceInfo`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_resource_info("res_type", "res_name", "owner", "description");
    /// ```
    pub fn set_resource_info(
        &mut self,
        resource_type: impl Into<String>,
        resource_name: impl Into<String>,
        owner: impl Into<String>,
        description: impl Into<String>,
    ) -> &mut Self {
        self.resource_info = Some(ResourceInfo::new(
            resource_type,
            resource_name,
            owner,
            description,
        ));
        self
    }

    /// Set [`Help`] details. Can be chained with other `.set_` and `.add_`
    /// [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails, HelpLink};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_help(vec![
    ///     HelpLink::new("description of link a", "resource-a.example.local"),
    ///     HelpLink::new("description of link b", "resource-b.example.local"),
    /// ]);
    /// ```
    pub fn set_help(&mut self, links: Vec<HelpLink>) -> &mut Self {
        self.help = Some(Help::new(links));
        self
    }

    /// Adds a [`HelpLink`] to [`Help`] details. Sets [`Help`] details if it is
    /// not set yet. Can be chained with other `.set_` and `.add_`
    /// [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.add_help_link("description of link", "resource.example.local");
    /// ```
    pub fn add_help_link(
        &mut self,
        description: impl Into<String>,
        url: impl Into<String>,
    ) -> &mut Self {
        match &mut self.help {
            Some(help) => {
                help.add_link(description, url);
            }
            None => {
                self.help = Some(Help::with_link(description, url));
            }
        };
        self
    }

    /// Returns `true` if [`Help`] is set and its `links` vector is not empty,
    /// otherwise returns `false`.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::with_help(vec![]);
    ///
    /// assert_eq!(err_details.has_help_links(), false);
    ///
    /// err_details.add_help_link("description of link", "resource.example.local");
    ///
    /// assert_eq!(err_details.has_help_links(), true);
    /// ```
    pub fn has_help_links(&self) -> bool {
        if let Some(help) = &self.help {
            return !help.links.is_empty();
        }
        false
    }

    /// Set [`LocalizedMessage`] details. Can be chained with other `.set_` and
    /// `.add_` [`ErrorDetails`] methods.
    /// # Examples
    ///
    /// ```
    /// use tonic_richer_error::{ErrorDetails};
    ///
    /// let mut err_details = ErrorDetails::new();
    ///
    /// err_details.set_localized_message("en-US", "message for the user");
    /// ```
    pub fn set_localized_message(
        &mut self,
        locale: impl Into<String>,
        message: impl Into<String>,
    ) -> &mut Self {
        self.localized_message = Some(LocalizedMessage::new(locale, message));
        self
    }
}
