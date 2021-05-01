//! HTTP specific body utilities.

pub(crate) use crate::codegen::empty_body;

/// A type erased HTTP body used for tonic services.
pub type BoxBody = http_body::combinators::BoxBody<bytes::Bytes, crate::Status>;
