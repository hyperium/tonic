use super::super::std_messages::{BadRequest, DebugInfo, RetryInfo};

/// Wraps the structs corresponding to the standard error messages, allowing
/// the implementation and handling of vectors containing any of them.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum ErrorDetail {
    /// Wraps the [`RetryInfo`] struct.
    RetryInfo(RetryInfo),

    /// Wraps the [`DebugInfo`] struct.
    DebugInfo(DebugInfo),

    /// Wraps the [`BadRequest`] struct.
    BadRequest(BadRequest),
}

impl From<RetryInfo> for ErrorDetail {
    fn from(err_detail: RetryInfo) -> Self {
        ErrorDetail::RetryInfo(err_detail)
    }
}

impl From<DebugInfo> for ErrorDetail {
    fn from(err_detail: DebugInfo) -> Self {
        ErrorDetail::DebugInfo(err_detail)
    }
}

impl From<BadRequest> for ErrorDetail {
    fn from(err_detail: BadRequest) -> Self {
        ErrorDetail::BadRequest(err_detail)
    }
}
