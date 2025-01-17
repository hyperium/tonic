use prost::DecodeError;

use crate::pb;

use super::*;

/// Used to implement associated functions and methods on `pb::Status`, that
/// allow the extraction of standard error details. This trait is
/// sealed and not meant to be implemented outside of `tonic-types`.
pub trait RpcStatusExt: crate::sealed::Sealed {
    /// Can be used to check if the error details contained in `pb::Status`
    /// are malformed or not. Tries to get an [`ErrorDetails`] struct from a
    /// `pb::Status`. If some `prost::DecodeError` occurs, it will be
    /// returned. If not debugging, consider using
    /// [`RpcStatusExt::get_error_details`] or
    /// [`RpcStatusExt::get_error_details_vec`].
    fn check_error_details(&self) -> Result<ErrorDetails, DecodeError>;

    /// Get an [`ErrorDetails`] struct from `pb::Status`. If some
    /// `prost::DecodeError` occurs, an empty [`ErrorDetails`] struct will be
    /// returned.
    fn get_error_details(&self) -> ErrorDetails;

    /// Can be used to check if the error details contained in `pb::Status`
    /// are malformed or not. Tries to get a vector of [`ErrorDetail`] enums
    /// from a `pb::Status`. If some `prost::DecodeError` occurs, it will be
    /// returned. If not debugging, consider using
    /// [`StatusExt::get_error_details_vec`] or
    /// [`StatusExt::get_error_details`].
    fn check_error_details_vec(&self) -> Result<Vec<ErrorDetail>, DecodeError>;

    /// Get a vector of [`ErrorDetail`] enums from `pb::Status`. If some
    /// `prost::DecodeError` occurs, an empty vector will be returned.
    fn get_error_details_vec(&self) -> Vec<ErrorDetail>;

    /// Get first [`RetryInfo`] details found on `pb::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    fn get_details_retry_info(&self) -> Option<RetryInfo>;

    /// Get first [`DebugInfo`] details found on `pb::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    fn get_details_debug_info(&self) -> Option<DebugInfo>;

    /// Get first [`QuotaFailure`] details found on `pb::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    fn get_details_quota_failure(&self) -> Option<QuotaFailure>;

    /// Get first [`ErrorInfo`] details found on `pb::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    fn get_details_error_info(&self) -> Option<ErrorInfo>;

    /// Get first [`PreconditionFailure`] details found on `pb::Status`,
    /// if any. If some `prost::DecodeError` occurs, returns `None`.
    fn get_details_precondition_failure(&self) -> Option<PreconditionFailure>;

    /// Get first [`BadRequest`] details found on `pb::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    fn get_details_bad_request(&self) -> Option<BadRequest>;

    /// Get first [`RequestInfo`] details found on `pb::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    fn get_details_request_info(&self) -> Option<RequestInfo>;

    /// Get first [`ResourceInfo`] details found on `pb::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    fn get_details_resource_info(&self) -> Option<ResourceInfo>;

    /// Get first [`Help`] details found on `pb::Status`, if any. If some
    /// `prost::DecodeError` occurs, returns `None`.
    fn get_details_help(&self) -> Option<Help>;

    /// Get first [`LocalizedMessage`] details found on `pb::Status`, if
    /// any. If some `prost::DecodeError` occurs, returns `None`.
    fn get_details_localized_message(&self) -> Option<LocalizedMessage>;
}

impl RpcStatusExt for pb::Status {
    fn check_error_details(&self) -> Result<ErrorDetails, DecodeError> {
        let mut details = ErrorDetails::new();

        for any in self.details.iter() {
            match any.type_url.as_str() {
                RetryInfo::TYPE_URL => {
                    details.retry_info = Some(RetryInfo::from_any_ref(any)?);
                }
                DebugInfo::TYPE_URL => {
                    details.debug_info = Some(DebugInfo::from_any_ref(any)?);
                }
                QuotaFailure::TYPE_URL => {
                    details.quota_failure = Some(QuotaFailure::from_any_ref(any)?);
                }
                ErrorInfo::TYPE_URL => {
                    details.error_info = Some(ErrorInfo::from_any_ref(any)?);
                }
                PreconditionFailure::TYPE_URL => {
                    details.precondition_failure = Some(PreconditionFailure::from_any_ref(any)?);
                }
                BadRequest::TYPE_URL => {
                    details.bad_request = Some(BadRequest::from_any_ref(any)?);
                }
                RequestInfo::TYPE_URL => {
                    details.request_info = Some(RequestInfo::from_any_ref(any)?);
                }
                ResourceInfo::TYPE_URL => {
                    details.resource_info = Some(ResourceInfo::from_any_ref(any)?);
                }
                Help::TYPE_URL => {
                    details.help = Some(Help::from_any_ref(any)?);
                }
                LocalizedMessage::TYPE_URL => {
                    details.localized_message = Some(LocalizedMessage::from_any_ref(any)?);
                }
                _ => {}
            }
        }

        Ok(details)
    }

    fn get_error_details(&self) -> ErrorDetails {
        self.check_error_details().unwrap_or_default()
    }

    fn check_error_details_vec(&self) -> Result<Vec<ErrorDetail>, DecodeError> {
        let mut details: Vec<ErrorDetail> = Vec::with_capacity(self.details.len());

        for any in self.details.iter() {
            match any.type_url.as_str() {
                RetryInfo::TYPE_URL => {
                    details.push(RetryInfo::from_any_ref(any)?.into());
                }
                DebugInfo::TYPE_URL => {
                    details.push(DebugInfo::from_any_ref(any)?.into());
                }
                QuotaFailure::TYPE_URL => {
                    details.push(QuotaFailure::from_any_ref(any)?.into());
                }
                ErrorInfo::TYPE_URL => {
                    details.push(ErrorInfo::from_any_ref(any)?.into());
                }
                PreconditionFailure::TYPE_URL => {
                    details.push(PreconditionFailure::from_any_ref(any)?.into());
                }
                BadRequest::TYPE_URL => {
                    details.push(BadRequest::from_any_ref(any)?.into());
                }
                RequestInfo::TYPE_URL => {
                    details.push(RequestInfo::from_any_ref(any)?.into());
                }
                ResourceInfo::TYPE_URL => {
                    details.push(ResourceInfo::from_any_ref(any)?.into());
                }
                Help::TYPE_URL => {
                    details.push(Help::from_any_ref(any)?.into());
                }
                LocalizedMessage::TYPE_URL => {
                    details.push(LocalizedMessage::from_any_ref(any)?.into());
                }
                _ => {}
            }
        }

        Ok(details)
    }

    fn get_error_details_vec(&self) -> Vec<ErrorDetail> {
        self.check_error_details_vec().unwrap_or_default()
    }

    fn get_details_retry_info(&self) -> Option<RetryInfo> {
        for any in self.details.iter() {
            if any.type_url.as_str() == RetryInfo::TYPE_URL {
                if let Ok(detail) = RetryInfo::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_debug_info(&self) -> Option<DebugInfo> {
        for any in self.details.iter() {
            if any.type_url.as_str() == DebugInfo::TYPE_URL {
                if let Ok(detail) = DebugInfo::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_quota_failure(&self) -> Option<QuotaFailure> {
        for any in self.details.iter() {
            if any.type_url.as_str() == QuotaFailure::TYPE_URL {
                if let Ok(detail) = QuotaFailure::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_error_info(&self) -> Option<ErrorInfo> {
        for any in self.details.iter() {
            if any.type_url.as_str() == ErrorInfo::TYPE_URL {
                if let Ok(detail) = ErrorInfo::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_precondition_failure(&self) -> Option<PreconditionFailure> {
        for any in self.details.iter() {
            if any.type_url.as_str() == PreconditionFailure::TYPE_URL {
                if let Ok(detail) = PreconditionFailure::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_bad_request(&self) -> Option<BadRequest> {
        for any in self.details.iter() {
            if any.type_url.as_str() == BadRequest::TYPE_URL {
                if let Ok(detail) = BadRequest::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_request_info(&self) -> Option<RequestInfo> {
        for any in self.details.iter() {
            if any.type_url.as_str() == RequestInfo::TYPE_URL {
                if let Ok(detail) = RequestInfo::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_resource_info(&self) -> Option<ResourceInfo> {
        for any in self.details.iter() {
            if any.type_url.as_str() == ResourceInfo::TYPE_URL {
                if let Ok(detail) = ResourceInfo::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_help(&self) -> Option<Help> {
        for any in self.details.iter() {
            if any.type_url.as_str() == Help::TYPE_URL {
                if let Ok(detail) = Help::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }

    fn get_details_localized_message(&self) -> Option<LocalizedMessage> {
        for any in self.details.iter() {
            if any.type_url.as_str() == LocalizedMessage::TYPE_URL {
                if let Ok(detail) = LocalizedMessage::from_any_ref(any) {
                    return Some(detail);
                }
            }
        }

        None
    }
}

impl crate::sealed::Sealed for pb::Status {}
