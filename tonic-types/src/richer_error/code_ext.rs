use http::status::StatusCode as HttpCode;
use tonic::Code;

/// Add gRPC Richer Error Model functionality to [`tonic::Code`].
pub trait CodeExt: crate::sealed::Sealed {
    /// gRPC to HTTP status code mappings as described in
    /// <https://cloud.google.com/apis/design/errors#generating_errors>.
    fn http_status(&self) -> http::status::StatusCode;
}

impl CodeExt for Code {
    fn http_status(&self) -> HttpCode {
        match self {
            Code::Ok => HttpCode::OK,
            Code::InvalidArgument | Code::FailedPrecondition | Code::OutOfRange => {
                HttpCode::BAD_REQUEST
            }
            Code::PermissionDenied => HttpCode::FORBIDDEN,
            Code::NotFound => HttpCode::NOT_FOUND,
            Code::Aborted | Code::AlreadyExists => HttpCode::CONFLICT,
            Code::Unauthenticated => HttpCode::UNAUTHORIZED,
            Code::ResourceExhausted => HttpCode::TOO_MANY_REQUESTS,
            Code::Cancelled => HttpCode::from_u16(499).expect("ivalid HTTP status code"),
            Code::DataLoss | Code::Unknown | Code::Internal => HttpCode::INTERNAL_SERVER_ERROR,
            Code::Unimplemented => HttpCode::NOT_IMPLEMENTED,
            Code::Unavailable => HttpCode::SERVICE_UNAVAILABLE,
            Code::DeadlineExceeded => HttpCode::GATEWAY_TIMEOUT,
        }
    }
}

impl crate::sealed::Sealed for Code {}
