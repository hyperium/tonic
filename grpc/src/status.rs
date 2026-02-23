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
