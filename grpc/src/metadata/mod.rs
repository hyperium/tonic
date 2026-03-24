/*
 *
 * Copyright 2026 gRPC authors.
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

//! Contains data structures and utilities for handling gRPC custom metadata.

mod encoding;
mod key;
mod map;
mod value;

pub use self::encoding::Ascii;
pub use self::encoding::Binary;
pub use self::key::AsciiMetadataKey;
pub use self::key::BinaryMetadataKey;
pub use self::key::MetadataKey;
pub use self::map::GetAll;
pub use self::map::Iter;
pub use self::map::Key;
pub use self::map::KeyAndValueRef;
pub use self::map::MetadataMap;
pub use self::map::ValueIter;
pub use self::value::AsciiMetadataValue;
pub use self::value::BinaryMetadataValue;
pub use self::value::MetadataValue;

/// The metadata::errors module contains types for errors that can occur
/// while handling gRPC custom metadata.
pub mod errors {
    pub use super::encoding::InvalidMetadataValue;
    pub use super::encoding::InvalidMetadataValueBytes;
    pub use super::key::InvalidMetadataKey;
    pub use super::value::ToStrError;
}
