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

/// Represents a gRPC status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum StatusCodeError {
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
}

impl From<i32> for StatusCodeError {
    fn from(i: i32) -> Self {
        match i {
            1 => StatusCodeError::Cancelled,
            2 => StatusCodeError::Unknown,
            3 => StatusCodeError::InvalidArgument,
            4 => StatusCodeError::DeadlineExceeded,
            5 => StatusCodeError::NotFound,
            6 => StatusCodeError::AlreadyExists,
            7 => StatusCodeError::PermissionDenied,
            8 => StatusCodeError::ResourceExhausted,
            9 => StatusCodeError::FailedPrecondition,
            10 => StatusCodeError::Aborted,
            11 => StatusCodeError::OutOfRange,
            12 => StatusCodeError::Unimplemented,
            13 => StatusCodeError::Internal,
            14 => StatusCodeError::Unavailable,
            15 => StatusCodeError::DataLoss,
            16 => StatusCodeError::Unauthenticated,
            _ => StatusCodeError::Unknown,
        }
    }
}
