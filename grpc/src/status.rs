/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

mod server_status;
mod status_code;

pub use server_status::ServerStatusErr;
pub use status_code::StatusCode;

/// Represents either a failing gRPC status or a successful result containing
/// `T`.
pub type StatusOr<T> = Result<T, StatusErr>;

/// The representation of a gRPC status.  OK statuses may not contain a status
/// message, while error values may.
pub type Status = StatusOr<()>;

/// Represents a gRPC status.
#[derive(Debug, Clone)]
pub struct StatusErr {
    code: StatusCode,
    message: String,
}

impl StatusErr {
    /// Create a new `StatusErr` with the given code and message.
    pub fn new(code: StatusCode, message: impl Into<String>) -> Self {
        StatusErr {
            code,
            message: message.into(),
        }
    }

    /// Get the `StatusCode` of this `StatusErr`.
    pub fn code(&self) -> StatusCode {
        self.code
    }

    /// Get the message of this `StatusErr`.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns whether the status includes a code restricted for control
    /// plane usage as defined by gRFC A54.
    pub(crate) fn is_restricted_control_plane_code(&self) -> bool {
        matches!(
            self.code,
            StatusCode::InvalidArgument
                | StatusCode::NotFound
                | StatusCode::AlreadyExists
                | StatusCode::FailedPrecondition
                | StatusCode::Aborted
                | StatusCode::OutOfRange
                | StatusCode::DataLoss
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_new() {
        let status = StatusErr::new(StatusCode::NotFound, "not ok");
        assert_eq!(status.code(), StatusCode::NotFound);
        assert_eq!(status.message(), "not ok");
    }

    #[test]
    fn test_status_debug() {
        let status = StatusErr::new(StatusCode::Cancelled, "not ok");
        let debug = format!("{:?}", status);
        assert!(debug.contains("Status"));
        assert!(debug.contains("Cancelled"));
        assert!(debug.contains("not ok"));
    }
}
