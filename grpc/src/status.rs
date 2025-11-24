mod server_status;
mod status_code;

pub use server_status::ServerStatus;
pub use status_code::StatusCode;

/// Represents a gRPC status.
#[derive(Debug, Clone)]
pub struct Status {
    code: StatusCode,
    message: String,
}

impl Status {
    /// Create a new `Status` with the given code and message.
    pub fn new(code: StatusCode, message: impl Into<String>) -> Self {
        Status {
            code,
            message: message.into(),
        }
    }

    /// Get the `StatusCode` of this `Status`.
    pub fn code(&self) -> StatusCode {
        self.code
    }

    /// Get the message of this `Status`.
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_new() {
        let status = Status::new(StatusCode::Ok, "ok");
        assert_eq!(status.code(), StatusCode::Ok);
        assert_eq!(status.message(), "ok");
    }

    #[test]
    fn test_status_debug() {
        let status = Status::new(StatusCode::Ok, "ok");
        let debug = format!("{:?}", status);
        assert!(debug.contains("Status"));
        assert!(debug.contains("Ok"));
        assert!(debug.contains("ok"));
    }
}
