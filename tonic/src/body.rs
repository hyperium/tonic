//! HTTP specific body utilities.

use http_body_util::BodyExt;

/// A type erased HTTP body used for tonic services.
pub type BoxBody = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, crate::Status>;

/// Convert a [`http_body::Body`] into a [`BoxBody`].
pub fn boxed<B>(body: B) -> BoxBody
where
    B: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    B::Error: Into<crate::BoxError>,
{
    body.map_err(crate::Status::map_error).boxed_unsync()
}

/// Create an empty `BoxBody`
#[deprecated(since = "0.12.4", note = "use `BoxBody::default()` instead")]
pub fn empty_body() -> BoxBody {
    BoxBody::default()
}
