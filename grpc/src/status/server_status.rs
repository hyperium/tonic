use super::status_code::StatusCode;
use super::Status;

/// Represents a gRPC status on the server.
///
/// This is a separate type from `Status` to prevent accidental conversion and
/// leaking of sensitive information from the server to the client.
#[derive(Debug, Clone)]
pub struct ServerStatus(Status);

impl std::ops::Deref for ServerStatus {
    type Target = Status;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerStatus {
    /// Create a new `ServerStatus` with the given code and message.
    pub fn new(code: StatusCode, message: impl Into<String>) -> Self {
        ServerStatus(Status::new(code, message))
    }

    /// Converts the `ServerStatus` to a `Status` for client responses.
    pub(crate) fn to_status(self) -> Status {
        self.0
    }
}

impl From<Status> for ServerStatus {
    fn from(status: Status) -> Self {
        ServerStatus(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_status_new() {
        let status = ServerStatus::new(StatusCode::Ok, "ok");
        assert_eq!(status.code(), StatusCode::Ok);
        assert_eq!(status.message(), "ok");
    }

    #[test]
    fn test_server_status_deref() {
        let status = ServerStatus::new(StatusCode::Ok, "ok");
        assert_eq!(status.code(), StatusCode::Ok);
    }

    #[test]
    fn test_server_status_from_status() {
        let status = Status::new(StatusCode::Ok, "ok");
        let server_status: ServerStatus = status.into();
        assert_eq!(server_status.code(), StatusCode::Ok);
    }

    #[test]
    fn test_server_status_to_status() {
        let server_status = ServerStatus::new(StatusCode::Ok, "ok");
        let status = server_status.to_status();
        assert_eq!(status.code(), StatusCode::Ok);
    }
}
