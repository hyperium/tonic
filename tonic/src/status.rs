#![allow(dead_code)]

use bytes::Bytes;
use http::header::HeaderValue;
use http::{self, HeaderMap};
use percent_encoding::{percent_decode, percent_encode, EncodeSet, DEFAULT_ENCODE_SET};
use std::{error::Error, fmt};
use tracing::{debug, trace, warn};

const GRPC_STATUS_HEADER_CODE: &str = "grpc-status";
const GRPC_STATUS_MESSAGE_HEADER: &str = "grpc-message";
const GRPC_STATUS_DETAILS_HEADER: &str = "grpc-status-details-bin";

/// A gRPC "status" describing the result of an RPC call.
#[derive(Clone)]
pub struct Status {
    /// The gRPC status code, found in the `grpc-status` header.
    code: Code,
    /// A relevant error message, found in the `grpc-message` header.
    message: String,
    /// Binary opaque details, found in the `grpc-status-details-bin` header.
    details: Bytes,
}

/// gRPC status codes used by `Status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Ok = 0,
    Cancelled = 1,
    Unknown = 2,
    InvalidArgument = 3,
    DeadlineExceeded = 4,
    NotFound = 5,
    AlreadyExists = 6,
    PermissionDenied = 7,
    ResourceExhausted = 8,
    FailedPrecondition = 9,
    Aborted = 10,
    OutOfRange = 11,
    Unimplemented = 12,
    Internal = 13,
    Unavailable = 14,
    DataLoss = 15,
    Unauthenticated = 16,

    // New Codes may be added in the future, so never exhaustively match!
    #[doc(hidden)]
    __NonExhaustive,
}

// ===== impl Status =====

impl Status {
    /// Create a new `Status` with the associated code and message.
    pub fn new(code: Code, message: impl Into<String>) -> Status {
        Status {
            code,
            message: message.into(),
            details: Bytes::new(),
        }
    }

    // Deprecated: this constructor encourages creating statuses with no
    // message, hurting later debugging.
    #[doc(hidden)]
    #[deprecated(note = "use State::new")]
    pub fn with_code(code: Code) -> Status {
        Status::new(code, String::new())
    }

    // Deprecated: this constructor is overly long.
    #[doc(hidden)]
    #[deprecated(note = "use State::new")]
    pub fn with_code_and_message(code: Code, message: String) -> Status {
        Status::new(code, message)
    }

    // TODO: This should probably be made public eventually. Need to decide on
    // the exact argument type.
    #[cfg_attr(not(feature = "h2"), allow(dead_code))]
    pub(crate) fn from_error(err: &(dyn Error + 'static)) -> Status {
        Status::try_from_error(err).unwrap_or_else(|| Status::new(Code::Unknown, err.to_string()))
    }

    fn try_from_error(err: &(dyn Error + 'static)) -> Option<Status> {
        let mut cause = Some(err);

        while let Some(err) = cause {
            if let Some(status) = err.downcast_ref::<Status>() {
                return Some(Status {
                    code: status.code,
                    message: status.message.clone(),
                    details: status.details.clone(),
                });
            }

            #[cfg(feature = "h2")]
            {
                if let Some(h2) = err.downcast_ref::<h2::Error>() {
                    return Some(Status::from_h2_error(h2));
                }
            }

            cause = err.source();
        }

        None
    }

    #[cfg(feature = "h2")]
    fn from_h2_error(err: &h2::Error) -> Status {
        // See https://github.com/grpc/grpc/blob/3977c30/doc/PROTOCOL-HTTP2.md#errors
        let code = match err.reason() {
            Some(h2::Reason::NO_ERROR)
            | Some(h2::Reason::PROTOCOL_ERROR)
            | Some(h2::Reason::INTERNAL_ERROR)
            | Some(h2::Reason::FLOW_CONTROL_ERROR)
            | Some(h2::Reason::SETTINGS_TIMEOUT)
            | Some(h2::Reason::COMPRESSION_ERROR)
            | Some(h2::Reason::CONNECT_ERROR) => Code::Internal,
            Some(h2::Reason::REFUSED_STREAM) => Code::Unavailable,
            Some(h2::Reason::CANCEL) => Code::Cancelled,
            Some(h2::Reason::ENHANCE_YOUR_CALM) => Code::ResourceExhausted,
            Some(h2::Reason::INADEQUATE_SECURITY) => Code::PermissionDenied,

            _ => Code::Unknown,
        };

        Status::new(code, format!("h2 protocol error: {}", err))
    }

    #[cfg(feature = "h2")]
    fn to_h2_error(&self) -> h2::Error {
        // conservatively transform to h2 error codes...
        let reason = match self.code {
            Code::Cancelled => h2::Reason::CANCEL,
            _ => h2::Reason::INTERNAL_ERROR,
        };

        reason.into()
    }

    pub(crate) fn map_error<E>(err: E) -> Status
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        Status::from_error(&*err.into())
    }

    pub(crate) fn from_header_map(header_map: &HeaderMap) -> Option<Status> {
        header_map.get(GRPC_STATUS_HEADER_CODE).map(|code| {
            let code = Code::from_bytes(code.as_ref());
            let error_message = header_map
                .get(GRPC_STATUS_MESSAGE_HEADER)
                .map(|header| {
                    percent_decode(header.as_bytes())
                        .decode_utf8()
                        .map(|cow| cow.to_string())
                })
                .unwrap_or_else(|| Ok(String::new()));
            let details = header_map
                .get(GRPC_STATUS_DETAILS_HEADER)
                .map(|h| Bytes::from(h.as_bytes()))
                .unwrap_or_else(Bytes::new);
            match error_message {
                Ok(message) => Status {
                    code,
                    message,
                    details,
                },
                Err(err) => {
                    warn!("Error deserializing status message header: {}", err);
                    Status {
                        code: Code::Unknown,
                        message: format!("Error deserializing status message header: {}", err),
                        details,
                    }
                }
            }
        })
    }

    /// Get the gRPC `Code` of this `Status`.
    pub fn code(&self) -> Code {
        self.code
    }

    /// Get the text error message of this `Status`.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get the opaque error details of this `Status`.
    pub fn details(&self) -> &[u8] {
        &self.details
    }

    #[doc(hidden)]
    #[deprecated(note = "use Status::message")]
    pub fn error_message(&self) -> &str {
        &self.message
    }

    #[doc(hidden)]
    #[deprecated(note = "use Status::details")]
    pub fn binary_error_details(&self) -> &Bytes {
        &self.details
    }

    pub(crate) fn to_header_map(&self) -> Result<HeaderMap, Self> {
        let mut header_map = HeaderMap::with_capacity(3);
        self.add_header(&mut header_map)?;
        Ok(header_map)
    }

    pub(crate) fn add_header(&self, header_map: &mut HeaderMap) -> Result<(), Self> {
        header_map.insert(GRPC_STATUS_HEADER_CODE, self.code.to_header_value());

        if !self.message.is_empty() {
            let is_need_encode = self
                .message
                .as_bytes()
                .iter()
                .any(|&x| DEFAULT_ENCODE_SET.contains(x));
            let to_write = if is_need_encode {
                percent_encode(&self.message().as_bytes(), DEFAULT_ENCODE_SET)
                    .to_string()
                    .into()
            } else {
                Bytes::from(self.message().as_bytes())
            };

            header_map.insert(
                GRPC_STATUS_MESSAGE_HEADER,
                HeaderValue::from_shared(to_write).map_err(invalid_header_value_byte)?,
            );
        }

        if !self.details.is_empty() {
            header_map.insert(
                GRPC_STATUS_DETAILS_HEADER,
                HeaderValue::from_shared(self.details.clone())
                    .map_err(invalid_header_value_byte)?,
            );
        }

        Ok(())
    }
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A manual impl to reduce the noise of frequently empty fields.
        let mut builder = f.debug_struct("Status");

        builder.field("code", &self.code);

        if !self.message.is_empty() {
            builder.field("message", &self.message);
        }

        if !self.details.is_empty() {
            builder.field("details", &self.details);
        }

        builder.finish()
    }
}

fn invalid_header_value_byte<Error: fmt::Display>(err: Error) -> Status {
    debug!("Invalid header: {}", err);
    Status::new(
        Code::Internal,
        "Couldn't serialize non-text grpc status header".to_string(),
    )
}

#[cfg(feature = "h2")]
impl From<h2::Error> for Status {
    fn from(err: h2::Error) -> Self {
        Status::from_h2_error(&err)
    }
}

#[cfg(feature = "h2")]
impl From<Status> for h2::Error {
    fn from(status: Status) -> Self {
        status.to_h2_error()
    }
}

impl From<std::io::Error> for Status {
    fn from(_io: std::io::Error) -> Self {
        unimplemented!()
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "grpc-status: {:?}, grpc-message: {:?}",
            self.code(),
            self.message()
        )
    }
}

impl Error for Status {}

///
/// Take the `Status` value from `trailers` if it is available, else from `status_code`.
///
pub(crate) fn infer_grpc_status(
    trailers: Option<HeaderMap>,
    status_code: http::StatusCode,
) -> Result<(), Status> {
    if let Some(trailers) = trailers {
        if let Some(status) = Status::from_header_map(&trailers) {
            if status.code() == Code::Ok {
                return Ok(());
            } else {
                return Err(status);
            }
        }
    }
    trace!("trailers missing grpc-status");
    let code = match status_code {
        // Borrowed from https://github.com/grpc/grpc/blob/master/doc/http-grpc-status-mapping.md
        http::StatusCode::BAD_REQUEST => Code::Internal,
        http::StatusCode::UNAUTHORIZED => Code::Unauthenticated,
        http::StatusCode::FORBIDDEN => Code::PermissionDenied,
        http::StatusCode::NOT_FOUND => Code::Unimplemented,
        http::StatusCode::TOO_MANY_REQUESTS
        | http::StatusCode::BAD_GATEWAY
        | http::StatusCode::SERVICE_UNAVAILABLE
        | http::StatusCode::GATEWAY_TIMEOUT => Code::Unavailable,
        _ => Code::Unknown,
    };

    let msg = format!(
        "grpc-status header missing, mapped from HTTP status code {}",
        status_code.as_u16(),
    );
    let status = Status::new(code, msg);
    Err(status)
}

// ===== impl Code =====

impl Code {
    /// Get the `Code` that represents the integer, if known.
    ///
    /// If not known, returns `Code::Unknown` (surprise!).
    pub fn from_i32(i: i32) -> Code {
        Code::from(i)
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Code {
        match bytes.len() {
            1 => match bytes[0] {
                b'0' => Code::Ok,
                b'1' => Code::Cancelled,
                b'2' => Code::Unknown,
                b'3' => Code::InvalidArgument,
                b'4' => Code::DeadlineExceeded,
                b'5' => Code::NotFound,
                b'6' => Code::AlreadyExists,
                b'7' => Code::PermissionDenied,
                b'8' => Code::ResourceExhausted,
                b'9' => Code::FailedPrecondition,
                _ => Code::parse_err(),
            },
            2 => match (bytes[0], bytes[1]) {
                (b'1', b'0') => Code::Aborted,
                (b'1', b'1') => Code::OutOfRange,
                (b'1', b'2') => Code::Unimplemented,
                (b'1', b'3') => Code::Internal,
                (b'1', b'4') => Code::Unavailable,
                (b'1', b'5') => Code::DataLoss,
                (b'1', b'6') => Code::Unauthenticated,
                _ => Code::parse_err(),
            },
            _ => Code::parse_err(),
        }
    }

    fn to_header_value(&self) -> HeaderValue {
        match self {
            Code::Ok => HeaderValue::from_static("0"),
            Code::Cancelled => HeaderValue::from_static("1"),
            Code::Unknown => HeaderValue::from_static("2"),
            Code::InvalidArgument => HeaderValue::from_static("3"),
            Code::DeadlineExceeded => HeaderValue::from_static("4"),
            Code::NotFound => HeaderValue::from_static("5"),
            Code::AlreadyExists => HeaderValue::from_static("6"),
            Code::PermissionDenied => HeaderValue::from_static("7"),
            Code::ResourceExhausted => HeaderValue::from_static("8"),
            Code::FailedPrecondition => HeaderValue::from_static("9"),
            Code::Aborted => HeaderValue::from_static("10"),
            Code::OutOfRange => HeaderValue::from_static("11"),
            Code::Unimplemented => HeaderValue::from_static("12"),
            Code::Internal => HeaderValue::from_static("13"),
            Code::Unavailable => HeaderValue::from_static("14"),
            Code::DataLoss => HeaderValue::from_static("15"),
            Code::Unauthenticated => HeaderValue::from_static("16"),

            Code::__NonExhaustive => unreachable!("Code::__NonExhaustive"),
        }
    }

    #[allow(dead_code)]
    fn parse_err() -> Code {
        trace!("error parsing grpc-status");
        Code::Unknown
    }
}

impl From<i32> for Code {
    fn from(i: i32) -> Self {
        match i {
            0 => Code::Ok,
            1 => Code::Cancelled,
            2 => Code::Unknown,
            3 => Code::InvalidArgument,
            4 => Code::DeadlineExceeded,
            5 => Code::NotFound,
            6 => Code::AlreadyExists,
            7 => Code::PermissionDenied,
            8 => Code::ResourceExhausted,
            9 => Code::FailedPrecondition,
            10 => Code::Aborted,
            11 => Code::OutOfRange,
            12 => Code::Unimplemented,
            13 => Code::Internal,
            14 => Code::Unavailable,
            15 => Code::DataLoss,
            16 => Code::Unauthenticated,

            _ => Code::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[derive(Debug)]
    struct Nested(Error);

    impl fmt::Display for Nested {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "nested error: {}", self.0)
        }
    }

    impl std::error::Error for Nested {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&*self.0)
        }
    }

    #[test]
    fn from_error_status() {
        let orig = Status::new(Code::OutOfRange, "weeaboo");
        let found = Status::from_error(&orig);

        assert_eq!(orig.code(), found.code());
        assert_eq!(orig.message(), found.message());
    }

    #[test]
    fn from_error_unknown() {
        let orig: Error = "peek-a-boo".into();
        let found = Status::from_error(&*orig);

        assert_eq!(found.code(), Code::Unknown);
        assert_eq!(found.message(), orig.to_string());
    }

    #[test]
    fn from_error_nested() {
        let orig = Nested(Box::new(Status::new(Code::OutOfRange, "weeaboo")));
        let found = Status::from_error(&orig);

        assert_eq!(found.code(), Code::OutOfRange);
        assert_eq!(found.message(), "weeaboo");
    }

    #[test]
    #[cfg(feature = "h2")]
    fn from_error_h2() {
        let orig = h2::Error::from(h2::Reason::CANCEL);
        let found = Status::from_error(&orig);

        assert_eq!(found.code(), Code::Cancelled);
    }

    #[test]
    #[cfg(feature = "h2")]
    fn to_h2_error() {
        let orig = Status::new(Code::Cancelled, "stop eet!");
        let err = orig.to_h2_error();

        assert_eq!(err.reason(), Some(h2::Reason::CANCEL));
    }

    #[test]
    fn code_from_i32() {
        // This for loop should catch if we ever add a new variant and don't
        // update From<i32>.
        for i in 0..(Code::__NonExhaustive as i32) {
            let code = Code::from(i);
            assert_eq!(
                i, code as i32,
                "Code::from({}) returned {:?} which is {}",
                i, code, code as i32,
            );
        }

        assert_eq!(Code::from(-1), Code::Unknown);
        assert_eq!(Code::from(Code::__NonExhaustive as i32), Code::Unknown);
    }
}
