/*!
A collection of useful protobuf types that can be used with `tonic`.

This crate also introduces the [`WithErrorDetails`] trait and implements it in
[`tonic::Status`], allowing the implementation of the [gRPC Richer Error Model]
with [`tonic`] in a convenient way.

# Usage
Useful protobuf types are available through the `pb` module. They can be
imported and worked with directly. The [`WithErrorDetails`] trait adds
associated functions to [`tonic::Status`] that can be used on the server side
to create a status with error details, which can then be returned to the gRPC
client. Moreover, the trait also adds methods to [`tonic::Status`] that can be
used by a tonic client to extract error details, and handle them with ease.

# Getting Started
To build this crate you must have the Protocol Buffer Compiler, `protoc`,
installed. Instructions can be found [here][protoc-install].

```toml
[dependencies]
tonic = <tonic-version>
tonic-types = <tonic-types-version>
```

# Examples
The examples bellow cover a basic use case using the [gRPC Richer Error Model].
More complete server and client implementations can be found at the main repo
[examples] directory.

## Server Side: Generating [`tonic::Status`] with an [`ErrorDetails`] struct
```
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, WithErrorDetails};

# async fn endpoint() -> Result<tonic::Response<()>, Status> {
// ...
// Inside a gRPC server endpoint that returns `Result<Response<T>, Status>`

// Create empty `ErrorDetails` struct
let mut err_details = ErrorDetails::new();

// Add error details conditionally
# let some_condition = true;
if some_condition {
    err_details.add_bad_request_violation(
        "field_a",
        "description of why the field_a is invalid"
    );
}

# let other_condition = true;
if other_condition {
    err_details.add_bad_request_violation(
        "field_b",
        "description of why the field_b is invalid",
    );
}

// Check if any error details were set and return error status if so
if err_details.has_bad_request_violations() {

    // Add additional error details if necessary
    err_details
        .add_help_link("description of link", "https://resource.example.local")
        .set_localized_message("en-US", "message for the user");

    let status = Status::with_error_details(
        Code::InvalidArgument,
        "bad request",
        err_details,
    );

    return Err(status);
}

// Handle valid request
// ...

# Ok(tonic::Response::new(()))
# }
```

## Client Side: Extracting an [`ErrorDetails`] struct from `tonic::Status`
```
use tonic::{Response, Status};
use tonic_types::{WithErrorDetails};

// ...

// Where `req_result` was returned by a gRPC client endpoint method
fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    match req_result {
        Ok(response) => {
            // Handle successful response
        },
        Err(status) => {
            let err_details = status.get_error_details();
            if let Some(bad_request) = err_details.bad_request {
                // Handle bad_request details
            }
            if let Some(help) = err_details.help {
                // Handle help details
            }
            if let Some(localized_message) = err_details.localized_message {
                // Handle localized_message details
            }
        }
    };
}
```

## Send different standard error messages
Multiple examples are provided at the [`ErrorDetails`] doc. Instructions about
how to use the fields of the standard error message types correctly are
provided at [error_details.proto].

## Alternative `tonic::Status` associated functions and methods
In the [`WithErrorDetails`] doc, an alternative way of interacting with
[`tonic::Status`] is presented, using vectors of error details structs wrapped
with the [`ErrorDetail`] enum. This approach can provide more control over the
vector of standard error messages that will be generated or that was received,
if necessary. To see how to adopt this approach, please check the
[`WithErrorDetails::with_error_details_vec`] and
[`WithErrorDetails::get_error_details_vec`] docs, and also the main repo
[examples] directory.\

Besides that, multiple examples with alternative error details extraction
methods are provided in the [`WithErrorDetails`] doc, which can be specially
useful if only one type of standard error message is being handled by the
client. For example, using [`WithErrorDetails::get_details_bad_request`] is a
more direct way of extracting a [`BadRequest`] error message from
[`tonic::Status`].

[`tonic::Status`]: https://docs.rs/tonic/0.8.0/tonic/struct.Status.html
[`tonic`]: https://docs.rs/tonic/0.8.0/tonic/
[gRPC Richer Error Model]: https://www.grpc.io/docs/guides/error/
[protoc-install]: https://grpc.io/docs/protoc-installation/
[examples]: https://github.com/hyperium/tonic/tree/master/examples
[error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
*/

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
use tonic::{codegen::Bytes, Code};

/// Useful protobuf types
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/google.rpc.rs"));
}

pub use pb::Status;

mod error_details;
mod error_details_vec;
mod std_messages;

pub use std_messages::*;

pub use error_details::ErrorDetails;

pub use error_details_vec::ErrorDetail;

trait IntoAny {
    fn into_any(self) -> Any;
}

trait FromAny {
    fn from_any(any: Any) -> Result<Self, DecodeError>
    where
        Self: Sized;
}

/// Used to implement associated functions and methods on `tonic::Status`, that
/// allow the addition and extraction of standard error details.
pub trait WithErrorDetails {
    /// Generates a `tonic::Status` with error details obtained from an
    /// [`ErrorDetails`] struct.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Code, Status};
    /// use tonic_types::{ErrorDetails, WithErrorDetails};
    ///
    /// let status = Status::with_error_details(
    ///     Code::InvalidArgument,
    ///     "bad request",
    ///     ErrorDetails::with_bad_request_violation("field", "description"),
    /// );
    /// ```
    fn with_error_details(
        code: tonic::Code,
        message: impl Into<String>,
        details: ErrorDetails,
    ) -> tonic::Status;

    /// Generates a `tonic::Status` with error details provided in a vector of
    /// [`ErrorDetail`] enums.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Code, Status};
    /// use tonic_types::{BadRequest, WithErrorDetails};
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
        code: tonic::Code,
        message: impl Into<String>,
        details: Vec<ErrorDetail>,
    ) -> tonic::Status;

    /// Can be used to check if the error details contained in `tonic::Status`
    /// are malformed or not. Tries to get an [`ErrorDetails`] struct from a
    /// `tonic::Status`. If some `prost::DecodeError` occurs, it will be
    /// returned. If not debugging, consider using
    /// [`WithErrorDetails::get_error_details`] or
    /// [`WithErrorDetails::get_error_details_vec`].
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             let err_details = status.get_error_details();
    ///             if let Some(bad_request) = err_details.bad_request {
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
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             let err_details = status.get_error_details();
    ///             if let Some(bad_request) = err_details.bad_request {
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
    /// [`WithErrorDetails::get_error_details_vec`] or
    /// [`WithErrorDetails::get_error_details`].
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{ErrorDetail, WithErrorDetails};
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
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{ErrorDetail, WithErrorDetails};
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

    /// Get first [`RetryInfo`] details found on `tonic::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(retry_info) = status.get_details_retry_info() {
    ///                 // Handle retry_info details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_retry_info(&self) -> Option<RetryInfo>;

    /// Get first [`DebugInfo`] details found on `tonic::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(debug_info) = status.get_details_debug_info() {
    ///                 // Handle debug_info details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_debug_info(&self) -> Option<DebugInfo>;

    /// Get first [`QuotaFailure`] details found on `tonic::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(quota_failure) = status.get_details_quota_failure() {
    ///                 // Handle quota_failure details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_quota_failure(&self) -> Option<QuotaFailure>;

    /// Get first [`ErrorInfo`] details found on `tonic::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(error_info) = status.get_details_error_info() {
    ///                 // Handle error_info details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_error_info(&self) -> Option<ErrorInfo>;

    /// Get first [`PreconditionFailure`] details found on `tonic::Status`,
    /// if any. If some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(precondition_failure) = status.get_details_precondition_failure() {
    ///                 // Handle precondition_failure details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_precondition_failure(&self) -> Option<PreconditionFailure>;

    /// Get first [`BadRequest`] details found on `tonic::Status`, if any. If
    /// some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
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

    /// Get first [`RequestInfo`] details found on `tonic::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(request_info) = status.get_details_request_info() {
    ///                 // Handle request_info details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_request_info(&self) -> Option<RequestInfo>;

    /// Get first [`ResourceInfo`] details found on `tonic::Status`, if any.
    /// If some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(resource_info) = status.get_details_resource_info() {
    ///                 // Handle resource_info details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_resource_info(&self) -> Option<ResourceInfo>;

    /// Get first [`Help`] details found on `tonic::Status`, if any. If some
    /// `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(help) = status.get_details_help() {
    ///                 // Handle help details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_help(&self) -> Option<Help>;

    /// Get first [`LocalizedMessage`] details found on `tonic::Status`, if
    /// any. If some `prost::DecodeError` occurs, returns `None`.
    /// # Examples
    ///
    /// ```
    /// use tonic::{Status, Response};
    /// use tonic_types::{WithErrorDetails};
    ///
    /// fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    ///     match req_result {
    ///         Ok(_) => {},
    ///         Err(status) => {
    ///             if let Some(localized_message) = status.get_details_localized_message() {
    ///                 // Handle localized_message details
    ///             }
    ///         }
    ///     };
    /// }
    /// ```
    fn get_details_localized_message(&self) -> Option<LocalizedMessage>;
}

impl WithErrorDetails for tonic::Status {
    fn with_error_details(code: Code, message: impl Into<String>, details: ErrorDetails) -> Self {
        let message: String = message.into();

        let mut conv_details: Vec<Any> = Vec::with_capacity(10);

        if let Some(retry_info) = details.retry_info {
            conv_details.push(retry_info.into_any());
        }

        if let Some(debug_info) = details.debug_info {
            conv_details.push(debug_info.into_any());
        }

        if let Some(quota_failure) = details.quota_failure {
            conv_details.push(quota_failure.into_any());
        }

        if let Some(error_info) = details.error_info {
            conv_details.push(error_info.into_any());
        }

        if let Some(precondition_failure) = details.precondition_failure {
            conv_details.push(precondition_failure.into_any());
        }

        if let Some(bad_request) = details.bad_request {
            conv_details.push(bad_request.into_any());
        }

        if let Some(request_info) = details.request_info {
            conv_details.push(request_info.into_any());
        }

        if let Some(resource_info) = details.resource_info {
            conv_details.push(resource_info.into_any());
        }

        if let Some(help) = details.help {
            conv_details.push(help.into_any());
        }

        if let Some(localized_message) = details.localized_message {
            conv_details.push(localized_message.into_any());
        }

        let status = pb::Status {
            code: code as i32,
            message: message.clone(),
            details: conv_details,
        };

        tonic::Status::with_details(code, message, Bytes::from(status.encode_to_vec()))
    }

    fn with_error_details_vec(
        code: Code,
        message: impl Into<String>,
        details: Vec<ErrorDetail>,
    ) -> Self {
        let message: String = message.into();

        let mut conv_details: Vec<Any> = Vec::with_capacity(details.len());

        for error_detail in details.into_iter() {
            match error_detail {
                ErrorDetail::RetryInfo(retry_info) => {
                    conv_details.push(retry_info.into_any());
                }
                ErrorDetail::DebugInfo(debug_info) => {
                    conv_details.push(debug_info.into_any());
                }
                ErrorDetail::QuotaFailure(quota_failure) => {
                    conv_details.push(quota_failure.into_any());
                }
                ErrorDetail::ErrorInfo(error_info) => {
                    conv_details.push(error_info.into_any());
                }
                ErrorDetail::PreconditionFailure(prec_failure) => {
                    conv_details.push(prec_failure.into_any());
                }
                ErrorDetail::BadRequest(bad_req) => {
                    conv_details.push(bad_req.into_any());
                }
                ErrorDetail::RequestInfo(req_info) => {
                    conv_details.push(req_info.into_any());
                }
                ErrorDetail::ResourceInfo(res_info) => {
                    conv_details.push(res_info.into_any());
                }
                ErrorDetail::Help(help) => {
                    conv_details.push(help.into_any());
                }
                ErrorDetail::LocalizedMessage(loc_message) => {
                    conv_details.push(loc_message.into_any());
                }
            }
        }

        let status = pb::Status {
            code: code as i32,
            message: message.clone(),
            details: conv_details,
        };

        tonic::Status::with_details(code, message, Bytes::from(status.encode_to_vec()))
    }

    fn check_error_details(&self) -> Result<ErrorDetails, DecodeError> {
        let status = pb::Status::decode(self.details())?;

        let mut details = ErrorDetails::new();

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                RetryInfo::TYPE_URL => {
                    details.retry_info = Some(RetryInfo::from_any(any)?);
                }
                DebugInfo::TYPE_URL => {
                    details.debug_info = Some(DebugInfo::from_any(any)?);
                }
                QuotaFailure::TYPE_URL => {
                    details.quota_failure = Some(QuotaFailure::from_any(any)?);
                }
                ErrorInfo::TYPE_URL => {
                    details.error_info = Some(ErrorInfo::from_any(any)?);
                }
                PreconditionFailure::TYPE_URL => {
                    details.precondition_failure = Some(PreconditionFailure::from_any(any)?);
                }
                BadRequest::TYPE_URL => {
                    details.bad_request = Some(BadRequest::from_any(any)?);
                }
                RequestInfo::TYPE_URL => {
                    details.request_info = Some(RequestInfo::from_any(any)?);
                }
                ResourceInfo::TYPE_URL => {
                    details.resource_info = Some(ResourceInfo::from_any(any)?);
                }
                Help::TYPE_URL => {
                    details.help = Some(Help::from_any(any)?);
                }
                LocalizedMessage::TYPE_URL => {
                    details.localized_message = Some(LocalizedMessage::from_any(any)?);
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
                RetryInfo::TYPE_URL => {
                    details.push(RetryInfo::from_any(any)?.into());
                }
                DebugInfo::TYPE_URL => {
                    details.push(DebugInfo::from_any(any)?.into());
                }
                QuotaFailure::TYPE_URL => {
                    details.push(QuotaFailure::from_any(any)?.into());
                }
                ErrorInfo::TYPE_URL => {
                    details.push(ErrorInfo::from_any(any)?.into());
                }
                PreconditionFailure::TYPE_URL => {
                    details.push(PreconditionFailure::from_any(any)?.into());
                }
                BadRequest::TYPE_URL => {
                    details.push(BadRequest::from_any(any)?.into());
                }
                RequestInfo::TYPE_URL => {
                    details.push(RequestInfo::from_any(any)?.into());
                }
                ResourceInfo::TYPE_URL => {
                    details.push(ResourceInfo::from_any(any)?.into());
                }
                Help::TYPE_URL => {
                    details.push(Help::from_any(any)?.into());
                }
                LocalizedMessage::TYPE_URL => {
                    details.push(LocalizedMessage::from_any(any)?.into());
                }
                _ => {}
            }
        }

        Ok(details)
    }

    fn get_error_details_vec(&self) -> Vec<ErrorDetail> {
        self.check_error_details_vec().unwrap_or(Vec::new())
    }

    fn get_details_retry_info(&self) -> Option<RetryInfo> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                RetryInfo::TYPE_URL => match RetryInfo::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_debug_info(&self) -> Option<DebugInfo> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                DebugInfo::TYPE_URL => match DebugInfo::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_quota_failure(&self) -> Option<QuotaFailure> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                QuotaFailure::TYPE_URL => match QuotaFailure::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_error_info(&self) -> Option<ErrorInfo> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                ErrorInfo::TYPE_URL => match ErrorInfo::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_precondition_failure(&self) -> Option<PreconditionFailure> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                PreconditionFailure::TYPE_URL => match PreconditionFailure::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
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

    fn get_details_request_info(&self) -> Option<RequestInfo> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                RequestInfo::TYPE_URL => match RequestInfo::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_resource_info(&self) -> Option<ResourceInfo> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                ResourceInfo::TYPE_URL => match ResourceInfo::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_help(&self) -> Option<Help> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                Help::TYPE_URL => match Help::from_any(any) {
                    Ok(detail) => return Some(detail),
                    Err(_) => {}
                },
                _ => {}
            }
        }

        None
    }

    fn get_details_localized_message(&self) -> Option<LocalizedMessage> {
        let status = pb::Status::decode(self.details()).ok()?;

        for any in status.details.into_iter() {
            match any.type_url.as_str() {
                LocalizedMessage::TYPE_URL => match LocalizedMessage::from_any(any) {
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
    use std::collections::HashMap;
    use std::time::Duration;
    use tonic::{Code, Status};

    use super::{
        BadRequest, DebugInfo, ErrorDetails, ErrorInfo, Help, LocalizedMessage,
        PreconditionFailure, QuotaFailure, RequestInfo, ResourceInfo, RetryInfo, WithErrorDetails,
    };

    #[test]
    fn gen_status_with_details() {
        let mut metadata = HashMap::new();
        metadata.insert("limitPerRequest".to_string(), "100".into());

        let mut err_details = ErrorDetails::new();

        err_details
            .set_retry_info(Some(Duration::from_secs(5)))
            .set_debug_info(
                vec![
                    "trace3".to_string(),
                    "trace2".to_string(),
                    "trace1".to_string(),
                ],
                "details",
            )
            .add_quota_failure_violation("clientip:<ip address>", "description")
            .set_error_info("SOME_INFO", "example.local", metadata.clone())
            .add_precondition_failure_violation("TOS", "example.local", "description")
            .add_bad_request_violation("field", "description")
            .set_request_info("request-id", "some-request-data")
            .set_resource_info("resource-type", "resource-name", "owner", "description")
            .add_help_link("link to resource", "resource.example.local")
            .set_localized_message("en-US", "message for the user");

        let fmt_details = format!("{:?}", err_details);

        println!("{fmt_details}\n");

        let err_details_vec = vec![
            RetryInfo::new(Some(Duration::from_secs(5))).into(),
            DebugInfo::new(
                vec![
                    "trace3".to_string(),
                    "trace2".to_string(),
                    "trace1".to_string(),
                ],
                "details",
            )
            .into(),
            QuotaFailure::with_violation("clientip:<ip address>", "description").into(),
            ErrorInfo::new("SOME_INFO", "example.local", metadata).into(),
            PreconditionFailure::with_violation("TOS", "example.local", "description").into(),
            BadRequest::with_violation("field", "description").into(),
            RequestInfo::new("request-id", "some-request-data").into(),
            ResourceInfo::new("resource-type", "resource-name", "owner", "description").into(),
            Help::with_link("link to resource", "resource.example.local").into(),
            LocalizedMessage::new("en-US", "message for the user").into(),
        ];

        let fmt_details_vec = format!("{:?}", err_details_vec);

        println!("{fmt_details_vec}\n");

        let status_from_struct = Status::with_error_details(
            Code::InvalidArgument,
            "error with bad request details",
            err_details,
        );

        let fmt_status_with_details = format!("{:?}", status_from_struct);

        println!("{:?}\n", fmt_status_with_details);

        let status_from_vec = Status::with_error_details_vec(
            Code::InvalidArgument,
            "error with bad request details",
            err_details_vec,
        );

        let fmt_status_with_details_vec = format!("{:?}", status_from_vec);

        println!("{:?}\n", fmt_status_with_details_vec);

        let ext_details = match status_from_vec.check_error_details() {
            Ok(ext_details) => ext_details,
            Err(err) => panic!(
                "Error extracting details struct from status_from_vec: {:?}",
                err
            ),
        };

        let fmt_ext_details = format!("{:?}", ext_details);

        println!("{:?}\n", ext_details.debug_info);
        println!("{fmt_ext_details}\n");

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

        println!("fmt_ext_details_vec: {:?}\n", fmt_ext_details_vec);

        assert!(
            fmt_ext_details_vec.eq(&fmt_details_vec),
            "Extracted details vec differs from original details vec"
        );
    }
}
