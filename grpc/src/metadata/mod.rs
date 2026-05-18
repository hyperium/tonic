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

//! gRPC metadata handling.
//!
//! gRPC metadata is a map of keys to values, similar to HTTP headers.  This
//! module provides types for working with metadata, including ASCII and binary
//! values.
//!
//! # Key Concepts
//!
//! - **[`MetadataMap`]:** The main struct for holding metadata.
//! - **[`MetadataKey`]:** Represents a key in the metadata map.
//! - **[`MetadataValue`]:** Represents a value in the metadata map.

mod encoding;
mod key;
mod map;
mod value;

pub use encoding::Ascii;
pub use encoding::Binary;
pub use key::AsciiMetadataKey;
pub use key::BinaryMetadataKey;
pub use key::MetadataKey;
pub use map::GetAll;
pub use map::Iter;
pub use map::Key;
pub use map::KeyAndValueRef;
pub use map::MetadataMap;
pub use map::ValueIter;
pub use value::AsciiMetadataValue;
pub use value::BinaryMetadataValue;
pub use value::MetadataValue;

/// The metadata::errors module contains types for errors that can occur
/// while handling gRPC custom metadata.
pub mod errors {
    pub use super::encoding::InvalidMetadataValue;
    pub use super::encoding::InvalidMetadataValueBytes;
    pub use super::key::InvalidMetadataKey;
    pub use super::value::ToStrError;
}
