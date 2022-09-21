//! A collection of useful protobuf types that can be used with `tonic`.
//!
//! This crate also introduces the [`StatusExt`] trait and implements it in
//! [`tonic::Status`], allowing the implementation of the
//! [gRPC Richer Error Model] with [`tonic`] in a convenient way.
//!
//! [`tonic::Status`]: https://docs.rs/tonic/latest/tonic/struct.Status.html
//! [`tonic`]: https://docs.rs/tonic/latest/tonic/
//! [gRPC Richer Error Model]: https://www.grpc.io/docs/guides/error/

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
#![doc(html_root_url = "https://docs.rs/tonic-types/0.6.0")]
#![doc(issue_tracker_base_url = "https://github.com/hyperium/tonic/issues/")]

use prost::{DecodeError, Message};
use prost_types::Any;
use tonic::{codegen::Bytes, metadata::MetadataMap, Code};

/// Useful protobuf types
pub mod pb {
    include!("generated/google.rpc.rs");
}

pub use pb::Status;

mod error_details;
mod std_messages;

pub use error_details::{vec::ErrorDetail, ErrorDetails};
pub use std_messages::{BadRequest, FieldViolation};

trait IntoAny {
    fn into_any(self) -> Any;
}

trait FromAny {
    fn from_any(any: Any) -> Result<Self, DecodeError>
    where
        Self: Sized;
}

fn gen_details_bytes(code: Code, message: &String, details: Vec<Any>) -> Bytes {
    let status = pb::Status {
        code: code as i32,
        message: message.clone(),
        details,
    };

    Bytes::from(status.encode_to_vec())
}

/// Used to implement associated functions and methods on `tonic::Status`, that
/// allow the addition and extraction of standard error details.
pub trait StatusExt {
    /// Generates a `tonic::Status` with error details obtained from an
    /// [`ErrorDetails`] struct, and custom metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{metadata::MetadataMap, Code, Status};
    /// use tonic_types::{ErrorDetails, StatusExt};
    ///
    /// let status = Status::with_error_details_and_metadata(
    ///     Code::InvalidArgument,
    ///     "bad request",
    ///     ErrorDetails::with_bad_request_violation("field", "description"),
    ///     MetadataMap::new()
    /// );
    /// ```
    fn with_error_details_and_metadata(
        code: Code,
        message: impl Into<String>,
        details: ErrorDetails,
        metadata: MetadataMap,
    ) -> tonic::Status;

    /// Generates a `tonic::Status` with error details obtained from an
    /// [`ErrorDetails`] struct.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Code, Status};
    /// use tonic_types::{ErrorDetails, StatusExt};
    ///
    /// let status = Status::with_error_details(
    ///     Code::InvalidArgument,
    ///     "bad request",
    ///     ErrorDetails::with_bad_request_violation("field", "description"),
    /// );
    /// ```
    fn with_error_details(
        code: Code,
        message: impl Into<String>,
        details: ErrorDetails,
    ) -> tonic::Status;

    /// Generates a `tonic::Status` with error details provided in a vector of
    /// [`ErrorDetail`] enums, and custom metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{metadata::MetadataMap, Code, Status};
    /// use tonic_types::{BadRequest, StatusExt};
    ///
    /// let status = Status::with_error_details_vec_and_metadata(
    ///     Code::InvalidArgument,
    ///     "bad request",
    ///     vec![
    ///         BadRequest::with_violation("field", "description").into(),
    ///     ],
    ///     MetadataMap::new()
    /// );
    /// ```
    fn with_error_details_vec_and_metadata(
        code: Code,
        message: impl Into<String>,
        details: impl IntoIterator<Item = ErrorDetail>,
        metadata: MetadataMap,
    ) -> tonic::Status;

    /// Generates a `tonic::Status` with error details provided in a vector of
    /// [`ErrorDetail`] enums.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Code, Status};
    /// use tonic_types::{BadRequest, StatusExt};
    ///
    /// let status = Status::with_error_details_vec(
    ///     Code::InvalidArgument,
    ///     "bad request",
    ///     vec![
    ///         BadRequest::with_violation("field", "description").into(),
    ///     ]
    /// );
    /// ```
    fn with_error_details_vec(
        code: Code,
        message: impl Into<String>,
        details: impl IntoIterator<Item = ErrorDetail>,
    ) -> tonic::Status;

    /// Can be used to check if the error details contained in `tonic::Status`
    /// are malformed or not. Tries to get an [`ErrorDetails`] struct from a
    /// `tonic::Status`. If some `prost::DecodeError` occurs, it will be
    /// returned. If not debugging, consider using
    /// [`StatusExt::get_error_details`] or
    /// [`StatusExt::get_error_details_vec`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{StatusExt};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             let err_details = status.get_error_details();
    ///             if let Some(bad_request) = err_details.bad_request() {
    ///                 // Handle bad_request details
    ///             }
    ///             // ...
    ///         }
    ///     };
    /// }
    /// ```
    fn check_error_details(&self) -> Result<ErrorDetails, DecodeError>;

    /// Get an [`ErrorDetails`] struct from `tonic::Status`. If some
    /// `prost::DecodeError` occurs, an empty [`ErrorDetails`] struct will be
    /// returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{StatusExt};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             let err_details = status.get_error_details();
    ///             if let Some(bad_request) = err_details.bad_request() {
    ///                 // Handle bad_request details
    ///             }
    ///             // ...
    ///         }
    ///     };
    /// }
    /// ```
    fn get_error_details(&self) -> ErrorDetails;

    /// Can be used to check if the error details contained in `tonic::Status`
    /// are malformed or not. Tries to get a vector of [`ErrorDetail`] enums
    /// from a `tonic::Status`. If some `prost::DecodeError` occurs, it will be
    /// returned. If not debugging, consider using
    /// [`StatusExt::get_error_details_vec`] or
    /// [`StatusExt::get_error_details`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{ErrorDetail, StatusExt};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             match status.check_error_details_vec() {
    ///                 Ok(err_details) => {
    ///                     // Handle extracted details
    ///                 }
    ///                 Err(decode_error) => {
    ///                     // Handle decode_error
    ///                 }
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn check_error_details_vec(&self) -> Result<Vec<ErrorDetail>, DecodeError>;

    /// Get a vector of [`ErrorDetail`] enums from `tonic::Status`. If some
    /// `prost::DecodeError` occurs, an empty vector will be returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{ErrorDetail, StatusExt};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             let err_details = status.get_error_details_vec();
    ///             for err_detail in err_details.iter() {
    ///                  match err_detail {
    ///                     ErrorDetail::BadRequest(bad_request) => {
    ///                         // Handle bad_request details
    ///                     }
    ///                     // ...
    ///                     _ => {}
    ///                  }
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_error_details_vec(&self) -> Vec<ErrorDetail>;

    /// Get first [`BadRequest`] details found on `tonic::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{StatusExt};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(bad_request) = status.get_details_bad_request() {
    ///                 // Handle bad_request details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_bad_request(&self) -> Option<BadRequest>;
}

impl StatusExt for tonic::Status {
    fn with_error_details_and_metadata(
        code: Code,
        message: impl Into<String>,
        details: ErrorDetails,
        metadata: MetadataMap,
    ) -> Self {
        let message: String = message.into();

        let mut conv_details: Vec<Any> = Vec::with_capacity(10);

        if let Some(bad_request) = details.bad_request {
            conv_details.push(bad_request.into_any());
        }

        let details = gen_details_bytes(code, &message, conv_details);

        tonic::Status::with_details_and_metadata(code, message, details, metadata)
    }

    fn with_error_details(code: Code, message: impl Into<String>, details: ErrorDetails) -> Self {
        tonic::Status::with_error_details_and_metadata(code, message, details, MetadataMap::new())
    }

    fn with_error_details_vec_and_metadata(
        code: Code,
        message: impl Into<String>,
        details: impl IntoIterator<Item = ErrorDetail>,
        metadata: MetadataMap,
    ) -> Self {
        let message: String = message.into();

        let mut conv_details: Vec<Any> = Vec::new();

        for error_detail in details.into_iter() {
            match error_detail {
                ErrorDetail::BadRequest(bad_req) => {
                    conv_details.push(bad_req.into_any());
                }
            }
        }

        let details = gen_details_bytes(code, &message, conv_details);

        tonic::Status::with_details_and_metadata(code, message, details, metadata)
    }

    fn with_error_details_vec(
        code: Code,
        message: impl Into<String>,
        details: impl IntoIterator<Item = ErrorDetail>,
    ) -> Self {
        tonic::Status::with_error_details_vec_and_metadata(
            code,
            message,
            details,
            MetadataMap::new(),
        )
    }

    fn check_error_details(&self) -> Result<ErrorDetails, DecodeError> {
        let status = pb::Status::decode(self.details())?;

        let mut details = ErrorDetails::new();

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                BadRequest::TYPE_URL => {
                    details.bad_request = Some(BadRequest::from_any(any)?);
                }
                _ => {}
            }
        }

        Ok(details)
    }

    fn get_error_details(&self) -> ErrorDetails {
        self.check_error_details().unwrap_or(ErrorDetails::new())
    }

    fn check_error_details_vec(&self) -> Result<Vec<ErrorDetail>, DecodeError> {
        let status = pb::Status::decode(self.details())?;

        let mut details: Vec<ErrorDetail> = Vec::with_capacity(status.details.len());

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                BadRequest::TYPE_URL => {
                    details.push(BadRequest::from_any(any)?.into());
                }
                _ => {}
            }
        }

        Ok(details)
    }

    fn get_error_details_vec(&self) -> Vec<ErrorDetail> {
        self.check_error_details_vec().unwrap_or(Vec::new())
    }

    fn get_details_bad_request(&self) -> Option<BadRequest> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                BadRequest::TYPE_URL => match BadRequest::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use tonic::{Code, Status};

    use super::{BadRequest, ErrorDetails, StatusExt};

    #[test]
    fn gen_status_with_details() {
        let mut err_details = ErrorDetails::new();

        err_details.add_bad_request_violation("field", "description");

        let fmt_details = format!("{:?}", err_details);

        let err_details_vec = vec![BadRequest::with_violation("field", "description").into()];

        let fmt_details_vec = format!("{:?}", err_details_vec);

        let status_from_struct = Status::with_error_details(
            Code::InvalidArgument,
            "error with bad request details",
            err_details,
        );

        let status_from_vec = Status::with_error_details_vec(
            Code::InvalidArgument,
            "error with bad request details",
            err_details_vec,
        );

        let ext_details = match status_from_vec.check_error_details() {
            Ok(ext_details) => ext_details,
            Err(err) => panic!(
                "Error extracting details struct from status_from_vec: {:?}",
                err
            ),
        };

        let fmt_ext_details = format!("{:?}", ext_details);

        assert!(
            fmt_ext_details.eq(&fmt_details),
            "Extracted details struct differs from original details struct"
        );

        let ext_details_vec = match status_from_struct.check_error_details_vec() {
            Ok(ext_details) => ext_details,
            Err(err) => panic!(
                "Error extracting details_vec from status_from_struct: {:?}",
                err
            ),
        };

        let fmt_ext_details_vec = format!("{:?}", ext_details_vec);

        assert!(
            fmt_ext_details_vec.eq(&fmt_details_vec),
            "Extracted details vec differs from original details vec"
        );
    }
}
