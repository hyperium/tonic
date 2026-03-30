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

use std::error::Error;
use std::fmt;
use std::hash::Hash;

use base64::Engine as _;
use bytes::Bytes;

use crate::metadata::value::UnencodedHeaderValue;
use crate::private;

/// A possible error when converting a `MetadataValue` from a string or byte
/// slice.
#[derive(Debug, Hash)]
#[non_exhaustive]
pub struct InvalidMetadataValue {}

pub trait ValueEncoding: Clone + Eq + PartialEq + Hash {
    /// Returns true if the provided key is valid for this ValueEncoding type.
    /// For example, `Ascii::is_valid_key("a") == true`,
    /// `Ascii::is_valid_key("a-bin") == false`.
    fn is_valid_key(key: &str) -> bool;

    #[doc(hidden)]
    fn from_bytes(
        value: &[u8],
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes>;

    #[doc(hidden)]
    fn from_shared(
        value: Bytes,
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes>;

    #[doc(hidden)]
    fn from_static(value: &'static str, _: private::Internal) -> UnencodedHeaderValue;

    #[doc(hidden)]
    fn decode(value: &[u8], _: private::Internal) -> Result<Bytes, InvalidMetadataValueBytes>;

    #[doc(hidden)]
    fn encode(value: Bytes, _: private::Internal) -> Bytes;

    #[doc(hidden)]
    fn equals(a: &UnencodedHeaderValue, b: &[u8], _: private::Internal) -> bool;

    #[doc(hidden)]
    fn values_equal(
        a: &UnencodedHeaderValue,
        b: &UnencodedHeaderValue,
        _: private::Internal,
    ) -> bool;

    #[doc(hidden)]
    fn fmt(
        value: &UnencodedHeaderValue,
        f: &mut fmt::Formatter<'_>,
        _: private::Internal,
    ) -> fmt::Result;
}

/// gRPC metadata values can be either ASCII strings or binary. Note that only
/// visible ASCII characters (32-127) are permitted.
/// This type should never be instantiated -- in fact, it's impossible
/// to, because there are no variants to instantiate. Instead, it's just used as
/// a type parameter for [`MetadataKey`] and [`MetadataValue`].
///
/// [`MetadataKey`]: crate::metadata::MetadataKey
/// [`MetadataValue`]: crate::metadata::MetadataValue
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Ascii {}

impl Ascii {
    pub(crate) fn is_valid_value(key: impl AsRef<[u8]>) -> bool {
        // This array maps every byte (0-255) to a boolean (valid/invalid).
        static VALID_METADATA_VALUE_CHARS: [bool; 256] = {
            let mut table = [false; 256];

            let mut i = 0x20;
            while i <= 0x7E {
                table[i as usize] = true;
                i += 1;
            }
            table
        };
        let bytes = key.as_ref();

        for &b in bytes {
            if !VALID_METADATA_VALUE_CHARS[b as usize] {
                return false;
            }
        }
        true
    }
}

/// gRPC metadata values can be either ASCII strings or binary.
/// This type should never be instantiated -- in fact, it's impossible
/// to, because there are no variants to instantiate. Instead, it's just used as
/// a type parameter for [`MetadataKey`] and [`MetadataValue`].
///
/// [`MetadataKey`]: crate::metadata::MetadataKey
/// [`MetadataValue`]: crate::metadata::MetadataValue
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Binary {}

// ===== impl ValueEncoding =====

impl ValueEncoding for Ascii {
    fn is_valid_key(key: &str) -> bool {
        !key.ends_with("-bin") && is_valid_key(key)
    }

    fn from_bytes(
        value: &[u8],
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes> {
        let value = value.trim_ascii();

        if value.is_empty() || !Ascii::is_valid_value(value) {
            return Err(InvalidMetadataValueBytes::new());
        }
        Ok(UnencodedHeaderValue::from_bytes(Bytes::copy_from_slice(
            value,
        )))
    }

    fn from_shared(
        value: Bytes,
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes> {
        let slice = value.as_ref();
        let trimmed = slice.trim_ascii();
        if !Ascii::is_valid_value(trimmed) {
            return Err(InvalidMetadataValueBytes::new());
        }

        // If the length hasn't changed, we don't need to slice (saves a ref-count bump).
        if trimmed.len() == slice.len() {
            return Ok(UnencodedHeaderValue::from_bytes(value));
        }

        // Since 'trimmed' is a sub-slice of 'slice', we can calculate indices instantly.
        let start = trimmed.as_ptr() as usize - slice.as_ptr() as usize;
        let end = start + trimmed.len();

        // This creates a new 'Bytes' pointing to the same memory region.
        Ok(UnencodedHeaderValue::from_bytes(value.slice(start..end)))
    }

    fn from_static(value: &'static str, _: private::Internal) -> UnencodedHeaderValue {
        let value = value.trim_ascii();
        if !Ascii::is_valid_value(value) {
            panic!("Invalid ASCII metadata value: {}", value)
        }
        UnencodedHeaderValue::from_bytes(Bytes::from_static(value.as_bytes()))
    }

    fn decode(value: &[u8], _: private::Internal) -> Result<Bytes, InvalidMetadataValueBytes> {
        let value = value.trim_ascii();

        if value.is_empty() || !Ascii::is_valid_value(value) {
            return Err(InvalidMetadataValueBytes::new());
        }
        Ok(Bytes::copy_from_slice(value))
    }

    fn equals(a: &UnencodedHeaderValue, b: &[u8], _: private::Internal) -> bool {
        a.data.as_ref() == b
    }

    fn values_equal(
        a: &UnencodedHeaderValue,
        b: &UnencodedHeaderValue,
        _: private::Internal,
    ) -> bool {
        a == b
    }

    fn fmt(
        value: &UnencodedHeaderValue,
        f: &mut fmt::Formatter<'_>,
        _: private::Internal,
    ) -> fmt::Result {
        fmt::Debug::fmt(value, f)
    }

    fn encode(value: Bytes, _: private::Internal) -> Bytes {
        value
    }
}

fn is_valid_key(key: impl AsRef<[u8]>) -> bool {
    // This array maps every byte (0-255) to a boolean (valid/invalid).
    static VALID_METADATA_KEY_CHARS: [bool; 256] = {
        let mut table = [false; 256];

        // Valid: 0-9
        let mut i = b'0';
        while i <= b'9' {
            table[i as usize] = true;
            i += 1;
        }

        // Valid: a-z
        let mut i = b'a';
        while i <= b'z' {
            table[i as usize] = true;
            i += 1;
        }

        // Valid: special chars
        table[b'_' as usize] = true;
        table[b'-' as usize] = true;
        table[b'.' as usize] = true;

        table
    };
    let bytes = key.as_ref();
    if bytes.is_empty() {
        return false;
    }

    for &b in bytes {
        if !VALID_METADATA_KEY_CHARS[b as usize] {
            return false;
        }
    }
    true
}

impl ValueEncoding for Binary {
    fn is_valid_key(key: &str) -> bool {
        key.ends_with("-bin") && is_valid_key(key)
    }

    fn from_bytes(
        value: &[u8],
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes> {
        Ok(UnencodedHeaderValue::from_bytes(Bytes::copy_from_slice(
            value,
        )))
    }

    fn from_shared(
        value: Bytes,
        _: private::Internal,
    ) -> Result<UnencodedHeaderValue, InvalidMetadataValueBytes> {
        Ok(UnencodedHeaderValue::from_bytes(value))
    }

    fn from_static(value: &'static str, _: private::Internal) -> UnencodedHeaderValue {
        UnencodedHeaderValue::from_bytes(Bytes::from_static(value.as_ref()))
    }

    fn decode(value: &[u8], _: private::Internal) -> Result<Bytes, InvalidMetadataValueBytes> {
        base64_util::STANDARD
            .decode(value)
            .map(|bytes_vec| bytes_vec.into())
            .map_err(|_| InvalidMetadataValueBytes::new())
    }

    fn equals(a: &UnencodedHeaderValue, b: &[u8], _: private::Internal) -> bool {
        a.data.as_ref() == b
    }

    fn values_equal(
        a: &UnencodedHeaderValue,
        b: &UnencodedHeaderValue,
        _: private::Internal,
    ) -> bool {
        a.data == b.data
    }

    fn fmt(
        value: &UnencodedHeaderValue,
        f: &mut fmt::Formatter<'_>,
        _: private::Internal,
    ) -> fmt::Result {
        write!(f, "{:?}", value.data)
    }

    fn encode(value: Bytes, _: private::Internal) -> Bytes {
        let encoded_value: String = base64_util::STANDARD_NO_PAD.encode(value);
        Bytes::from(encoded_value)
    }
}

// ===== impl InvalidMetadataValue =====

impl InvalidMetadataValue {
    pub(crate) fn new() -> Self {
        InvalidMetadataValue {}
    }
}

impl fmt::Display for InvalidMetadataValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("failed to parse metadata value")
    }
}

impl Error for InvalidMetadataValue {}

/// A possible error when converting a `MetadataValue` from a string or byte
/// slice.
#[derive(Debug, Hash)]
pub struct InvalidMetadataValueBytes(InvalidMetadataValue);

// ===== impl InvalidMetadataValueBytes =====

impl InvalidMetadataValueBytes {
    pub(crate) fn new() -> Self {
        InvalidMetadataValueBytes(InvalidMetadataValue::new())
    }
}

impl fmt::Display for InvalidMetadataValueBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for InvalidMetadataValueBytes {}

mod base64_util {
    use base64::alphabet;
    use base64::engine::DecodePaddingMode;
    use base64::engine::general_purpose::GeneralPurpose;
    use base64::engine::general_purpose::GeneralPurposeConfig;

    pub(super) const STANDARD: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_encode_padding(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    pub(super) const STANDARD_NO_PAD: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
}
