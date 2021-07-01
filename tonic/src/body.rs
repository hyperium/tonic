//! HTTP specific body utilities.

use http_body::Body;

/// A type erased HTTP body used for tonic services.
pub type BoxBody = http_body::combinators::BoxBody<bytes::Bytes, crate::Status>;

// this also exists in `crate::codegen` but we need it here since `codegen` has
// `#[cfg(feature = "codegen")]`.
/// Create an empty `BoxBody`
pub fn empty_body() -> BoxBody {
    http_body::Empty::new().map_err(|err| match err {}).boxed()
}
