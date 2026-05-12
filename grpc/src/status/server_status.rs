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

use crate::status::StatusError;
use crate::status::status_code::StatusCodeError;

/// Represents a gRPC status on the server.
///
/// This is a separate type from `Status` to prevent accidental conversion and
/// leaking of sensitive information from the server to the client.
#[derive(Debug, Clone)]
pub struct ServerStatusErr(StatusError);

impl std::ops::Deref for ServerStatusErr {
    type Target = StatusError;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerStatusErr {
    /// Create a new `ServerStatus` with the given code and message.
    pub fn new(code: StatusCodeError, message: impl Into<String>) -> Self {
        ServerStatusErr(StatusError::new(code, message))
    }

    /// Create a new `ServerStatus` from a `Status`.
    pub fn from_status(status: StatusError) -> Self {
        ServerStatusErr(status)
    }

    /// Converts the `ServerStatus` to a `Status` for client responses.
    pub(crate) fn into_status(self) -> StatusError {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_status_new() {
        let status = ServerStatusErr::new(StatusCodeError::Internal, "not ok");
        assert_eq!(status.code(), StatusCodeError::Internal);
        assert_eq!(status.message(), "not ok");
    }

    #[test]
    fn test_server_status_deref() {
        let status = ServerStatusErr::new(StatusCodeError::FailedPrecondition, "x");
        assert_eq!(status.code(), StatusCodeError::FailedPrecondition);
    }

    #[test]
    fn test_server_status_from_status() {
        let status = StatusError::new(StatusCodeError::DeadlineExceeded, "DE");
        let server_status = ServerStatusErr::from_status(status);
        assert_eq!(server_status.code(), StatusCodeError::DeadlineExceeded);
    }

    #[test]
    fn test_server_status_into_status() {
        let server_status = ServerStatusErr::new(StatusCodeError::DataLoss, "DL");
        let status = server_status.into_status();
        assert_eq!(status.code(), StatusCodeError::DataLoss);
    }
}
