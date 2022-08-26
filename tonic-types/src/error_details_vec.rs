use super::std_messages::*;

/// Wraps the structs corresponding to the standard error messages, allowing
/// the implementation and handling of vectors containing any of them.
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
