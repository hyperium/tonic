//! HTTP specific body utilities.

use crate::codec::SliceBuffer;
use http_body::{combinators::UnsyncBoxBody, Body};

/// A type erased HTTP body used for tonic services.
pub type BoxBody = UnsyncBoxBody<SliceBuffer, crate::Status>;

/// Convert a [`http_body::Body`] into a [`BoxBody`].
pub(crate) fn boxed<B>(body: B) -> BoxBody
where
    B: http_body::Body + Send + 'static,
    B::Data: Into<SliceBuffer>,
    B::Error: Into<crate::Error>,
{
    body.map_data(Into::into)
        .map_err(crate::Status::map_error)
        .boxed_unsync()
}

/// Create an empty `BoxBody`
pub fn empty_body() -> BoxBody {
    http_body::Empty::new()
        .map_err(|err| match err {})
        .boxed_unsync()
}
