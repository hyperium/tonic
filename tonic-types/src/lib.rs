//! A collection of useful protobuf types that can be used with `tonic`.
//!
//! This crate also introduces the [`StatusExt`] trait and implements it in
//! [`tonic::Status`], allowing the implementation of the
//! [gRPC Richer Error Model] with [`tonic`] in a convenient way.
//!
//! # Usage
//!
//! Useful protobuf types are available through the [`pb`] module. They can be
//! imported and worked with directly.
//!
//! The [`StatusExt`] trait adds associated functions to [`tonic::Status`] that
//! can be used on the server side to create a status with error details, which
//! can then be returned to gRPC clients. Moreover, the trait also adds methods
//! to [`tonic::Status`] that can be used by a tonic client to extract error
//! details, and handle them with ease.
//!
//! # Getting Started
//!
//! ```toml
//! [dependencies]
//! tonic = <tonic-version>
//! tonic-types = <tonic-types-version>
//! ```
//!
//! # Examples
//!
//! The examples bellow cover a basic use case of the [gRPC Richer Error Model].
//! More complete server and client implementations are provided in the
//! **Richer Error example**, located in the main repo [examples] directory.
//!
//! ## Server Side: Generating [`tonic::Status`] with an [`ErrorDetails`] struct
//!
//! ```
//! use tonic::{Code, Status};
//! use tonic_types::{ErrorDetails, StatusExt};
//!
//! # async fn endpoint() -> Result<tonic::Response<()>, Status> {
//! // ...
//! // Inside a gRPC server endpoint that returns `Result<Response<T>, Status>`
//!
//! // Create empty `ErrorDetails` struct
//! let mut err_details = ErrorDetails::new();
//!
//! // Add error details conditionally
//! # let some_condition = true;
//! if some_condition {
//!     err_details.add_bad_request_violation(
//!         "field_a",
//!         "description of why the field_a is invalid"
//!     );
//! }
//!
//! # let other_condition = true;
//! if other_condition {
//!     err_details.add_bad_request_violation(
//!         "field_b",
//!         "description of why the field_b is invalid",
//!     );
//! }
//!
//! // Check if any error details were set and return error status if so
//! if err_details.has_bad_request_violations() {
//!     // Add additional error details if necessary
//!     err_details
//!         .add_help_link("description of link", "https://resource.example.local")
//!         .set_localized_message("en-US", "message for the user");
//!
//!     let status = Status::with_error_details(
//!         Code::InvalidArgument,
//!         "bad request",
//!         err_details,
//!     );
//!     return Err(status);
//! }
//!
//! // Handle valid request
//! // ...
//! # Ok(tonic::Response::new(()))
//! # }
//! ```
//!
//! ## Client Side: Extracting an [`ErrorDetails`] struct from `tonic::Status`
//!
//! ```
//! use tonic::{Response, Status};
//! use tonic_types::StatusExt;
//!
//! // ...
//! // Where `req_result` was returned by a gRPC client endpoint method
//! fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
//!     match req_result {
//!         Ok(response) => {
//!             // Handle successful response
//!         },
//!         Err(status) => {
//!             let err_details = status.get_error_details();
//!             if let Some(bad_request) = err_details.bad_request() {
//!                 // Handle bad_request details
//!             }
//!             if let Some(help) = err_details.help() {
//!                 // Handle help details
//!             }
//!             if let Some(localized_message) = err_details.localized_message() {
//!                 // Handle localized_message details
//!             }
//!         }
//!     };
//! }
//! ```
//!
//! # Working with different error message types
//!
//! Multiple examples are provided at the [`ErrorDetails`] doc. Instructions
//! about how to use the fields of the standard error message types correctly
//! are provided at [error_details.proto].
//!
//! # Alternative `tonic::Status` associated functions and methods
//!
//! In the [`StatusExt`] doc, an alternative way of interacting with
//! [`tonic::Status`] is presented, using vectors of error details structs
//! wrapped with the [`ErrorDetail`] enum. This approach can provide more
//! control over the vector of standard error messages that will be generated or
//! that was received, if necessary. To see how to adopt this approach, please
//! check the [`StatusExt::with_error_details_vec`] and
//! [`StatusExt::get_error_details_vec`] docs, and also the main repo's
//! [Richer Error example] directory.
//!
//! Besides that, multiple examples with alternative error details extraction
//! methods are provided in the [`StatusExt`] doc, which can be specially
//! useful if only one type of standard error message is being handled by the
//! client. For example, using [`StatusExt::get_details_bad_request`] is a
//! more direct way of extracting a [`BadRequest`] error message from
//! [`tonic::Status`].
//!
//! [`tonic::Status`]: https://docs.rs/tonic/latest/tonic/struct.Status.html
//! [`tonic`]: https://docs.rs/tonic/latest/tonic/
//! [gRPC Richer Error Model]: https://www.grpc.io/docs/guides/error/
//! [examples]: https://github.com/hyperium/tonic/tree/master/examples
//! [error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
//! [Richer Error example]: https://github.com/hyperium/tonic/tree/master/examples/src/richer-error

#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/tokio-rs/website/master/public/img/icons/tonic.svg"
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(html_root_url = "https://docs.rs/tonic-types/0.10.2")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

mod generated {
    #![allow(unreachable_pub)]
    #![allow(rustdoc::invalid_html_tags)]
    #[rustfmt::skip]
    pub mod google_rpc;

    /// Byte encoded FILE_DESCRIPTOR_SET.
    pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/types.bin");

    #[cfg(test)]
    mod tests {
        use super::FILE_DESCRIPTOR_SET;
        use prost::Message as _;

        #[test]
        fn file_descriptor_set_is_valid() {
            prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
        }
    }
}

/// Useful protobuf types
pub mod pb {
    pub use crate::generated::{google_rpc::*, FILE_DESCRIPTOR_SET};
}

pub use pb::Status;

mod richer_error;

pub use richer_error::{
    BadRequest, DebugInfo, ErrorDetail, ErrorDetails, ErrorInfo, FieldViolation, Help, HelpLink,
    LocalizedMessage, PreconditionFailure, PreconditionViolation, QuotaFailure, QuotaViolation,
    RequestInfo, ResourceInfo, RetryInfo, RpcStatusExt, StatusExt,
};

mod sealed {
    pub trait Sealed {}
}
