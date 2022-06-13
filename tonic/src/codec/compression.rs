use super::encode::BUFFER_SIZE;
use crate::{metadata::MetadataValue, Status};
use bytes::{Buf, BytesMut};
#[cfg(feature = "gzip")]
use flate2::read::{GzDecoder, GzEncoder};
use std::fmt;

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    #[cfg(feature = "gzip")]
    pub(crate) gzip: bool,
}

impl EnabledCompressionEncodings {
    /// Check if a [`CompressionEncoding`] is enabled.
    pub fn is_enabled(&self, encoding: CompressionEncoding) -> bool {
        match encoding {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => self.gzip,
        }
    }

    /// Enable a [`CompressionEncoding`].
    pub fn enable(&mut self, encoding: CompressionEncoding) {
        match encoding {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => self.gzip = true,
        }
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        if self.is_gzip_enabled() {
            Some(http::HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }

    #[cfg(feature = "gzip")]
    const fn is_gzip_enabled(&self) -> bool {
        self.gzip
    }

    #[cfg(not(feature = "gzip"))]
    const fn is_gzip_enabled(&self) -> bool {
        false
    }
}

/// The compression encodings Tonic supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionEncoding {
    #[allow(missing_docs)]
    #[cfg(feature = "gzip")]
    #[cfg_attr(docsrs, doc(cfg(feature = "gzip")))]
    Gzip,
}

impl CompressionEncoding {
    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub(crate) fn from_accept_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Option<Self> {
        if !enabled_encodings.is_gzip_enabled() {
            return None;
        }

        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        split_by_comma(header_value_str).find_map(|value| match value {
            #[cfg(feature = "gzip")]
            "gzip" => Some(CompressionEncoding::Gzip),
            _ => None,
        })
    }

    /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't supported.
    pub(crate) fn from_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Result<Option<Self>, Status> {
        let header_value = if let Some(value) = map.get(ENCODING_HEADER) {
            value
        } else {
            return Ok(None);
        };

        let header_value_str = if let Ok(value) = header_value.to_str() {
            value
        } else {
            return Ok(None);
        };

        match header_value_str {
            #[cfg(feature = "gzip")]
            "gzip" if enabled_encodings.is_enabled(CompressionEncoding::Gzip) => {
                Ok(Some(CompressionEncoding::Gzip))
            }
            "identity" => Ok(None),
            other => {
                let mut status = Status::unimplemented(format!(
                    "Content is compressed with `{}` which isn't supported",
                    other
                ));

                let header_value = enabled_encodings
                    .into_accept_encoding_header_value()
                    .map(MetadataValue::unchecked_from_header_value)
                    .unwrap_or_else(|| MetadataValue::from_static("identity"));
                status
                    .metadata_mut()
                    .insert(ACCEPT_ENCODING_HEADER, header_value);

                Err(status)
            }
        }
    }

    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => http::HeaderValue::from_static("gzip"),
        }
    }

    pub(crate) fn encodings() -> &'static [Self] {
        &[
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip,
        ]
    }
}

impl fmt::Display for CompressionEncoding {
    #[allow(unused_variables)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => write!(f, "gzip"),
        }
    }
}

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.trim().split(',').map(|s| s.trim())
}

/// Compress `len` bytes from `decompressed_buf` into `out_buf`.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn compress(
    encoding: CompressionEncoding,
    decompressed_buf: &mut BytesMut,
    out_buf: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buf.reserve(capacity);

    match encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            let mut gzip_encoder = GzEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            let mut out_writer = bytes::BufMut::writer(out_buf);

            std::io::copy(&mut gzip_encoder, &mut out_writer)?;
        }
    }

    decompressed_buf.advance(len);

    Ok(())
}

/// Decompress `len` bytes from `compressed_buf` into `out_buf`.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn decompress(
    encoding: CompressionEncoding,
    compressed_buf: &mut BytesMut,
    out_buf: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let estimate_decompressed_len = len * 2;
    let capacity = ((estimate_decompressed_len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buf.reserve(capacity);

    match encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            let mut gzip_decoder = GzDecoder::new(&compressed_buf[0..len]);
            let mut out_writer = bytes::BufMut::writer(out_buf);

            std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        }
    }

    compressed_buf.advance(len);

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SingleMessageCompressionOverride {
    /// Inherit whatever compression is already configured. If the stream is compressed this
    /// message will also be configured.
    ///
    /// This is the default.
    Inherit,
    /// Don't compress this message, even if compression is enabled on the stream.
    Disable,
}

impl Default for SingleMessageCompressionOverride {
    fn default() -> Self {
        Self::Inherit
    }
}
