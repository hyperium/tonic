use bytes::{Buf, BytesMut};
use std::fmt::Debug;
use tracing::debug;

use super::{
    compressors::{self, IDENTITY},
    Compressor, DecompressionError, ENCODING_HEADER,
};

const BUFFER_SIZE: usize = 8 * 1024;

/// Information related to the decompression of a request or response
pub struct Decompression {
    encoding: Option<String>,
    compressor: Option<&'static Box<dyn Compressor>>,
}

impl Debug for Decompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let encoding = self.encoding.as_ref().map(|e| &e[..]).unwrap_or("");
        f.debug_struct("Compression")
            .field("encoding", &encoding)
            .field(
                "compressor",
                &self.compressor.map(|c| c.name()).unwrap_or(""),
            )
            .finish()
    }
}

impl Decompression {
    /// Create a `Decompression` structure from an encoding name
    pub fn from_encoding(encoding: Option<&str>) -> Decompression {
        let compressor = encoding.and_then(compressors::get);

        Decompression {
            encoding: encoding.map(|v| v.to_string()),
            compressor,
        }
    }

    /// Create a `Decompression` structure from http headers
    pub fn from_headers(metadata: &http::HeaderMap) -> Decompression {
        let encoding = metadata
            .get(ENCODING_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| if v == IDENTITY { None } else { Some(v) });

        Decompression::from_encoding(encoding)
    }

    /// Decompress `len` bytes from `in_buffer` into `out_buffer`
    pub fn decompress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> Result<(), DecompressionError> {
        let compressor = self.compressor.ok_or_else(|| {
            match &self.encoding {
                // Asked to decompress but not compression was specified
                None => DecompressionError::NoCompression,
                // Asked to decompress but the decompressor wasn't found
                Some(encoding) => DecompressionError::NotFound {
                    requested: encoding.clone(),
                    known: compressors::names(),
                },
            }
        })?;

        let capacity =
            ((compressor.estimate_decompressed_len(len) / BUFFER_SIZE) + 1) * BUFFER_SIZE;
        out_buffer.reserve(capacity);
        compressor.decompress(in_buffer, out_buffer, len)?;
        in_buffer.advance(len);

        debug!(
            "Decompressed {} bytes into {} bytes using {:?}",
            len,
            out_buffer.len(),
            compressor.name()
        );
        Ok(())
    }
}

impl Default for Decompression {
    fn default() -> Self {
        Decompression {
            encoding: None,
            compressor: None,
        }
    }
}
