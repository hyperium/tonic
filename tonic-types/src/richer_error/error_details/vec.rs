use super::super::std_messages::BadRequest;

/// Wraps the structs corresponding to the standard error messages, allowing
/// the implementation and handling of vectors containing any of them.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum ErrorDetail {
    /// Wraps the [`BadRequest`] struct.
    BadRequest(BadRequest),
}

impl From<BadRequest> for ErrorDetail {
    fn from(err_detail: BadRequest) -> Self {
        ErrorDetail::BadRequest(err_detail)
    }
}
