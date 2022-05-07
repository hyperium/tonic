use super::encode::BUFFER_SIZE;
use crate::{metadata::MetadataValue, Status};
use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};
use std::fmt;

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    pub(crate) gzip: bool,
}

impl EnabledCompressionEncodings {
    /// Check if `gzip` compression is enabled.
    pub fn gzip(self) -> bool {
        self.gzip
    }

    /// Enable `gzip` compression.
    pub fn enable_gzip(&mut self) {
        self.gzip = true;
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        let Self { gzip } = self;
        if gzip {
            Some(http::HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }
}

/// The compression encodings Tonic supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionEncoding {
    #[allow(missing_docs)]
    Gzip,
}

impl CompressionEncoding {
    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub(crate) fn from_accept_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Option<Self> {
        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        let EnabledCompressionEncodings { gzip } = enabled_encodings;

        split_by_comma(header_value_str).find_map(|value| match value {
            "gzip" if gzip => Some(CompressionEncoding::Gzip),
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

        let EnabledCompressionEncodings { gzip } = enabled_encodings;

        match header_value_str {
            "gzip" if gzip => Ok(Some(CompressionEncoding::Gzip)),
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
            CompressionEncoding::Gzip => http::HeaderValue::from_static("gzip"),
        }
    }
}

impl fmt::Display for CompressionEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompressionEncoding::Gzip => write!(f, "gzip"),
        }
    }
}

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.trim().split(',').map(|s| s.trim())
}

/// Compress `len` bytes from `decompressed_buf` into `out_buf`.
pub(crate) fn compress(
    encoding: CompressionEncoding,
    decompressed_buf: &mut BytesMut,
    out_buf: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buf.reserve(capacity);

    match encoding {
        CompressionEncoding::Gzip => {
            let mut gzip_encoder = GzEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            let mut out_writer = out_buf.writer();

            std::io::copy(&mut gzip_encoder, &mut out_writer)?;
        }
    }

    decompressed_buf.advance(len);

    Ok(())
}

/// Decompress `len` bytes from `compressed_buf` into `out_buf`.
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
        CompressionEncoding::Gzip => {
            let mut gzip_decoder = GzDecoder::new(&compressed_buf[0..len]);
            let mut out_writer = out_buf.writer();

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
