use bytes::{Buf, BytesMut};
use tracing::debug;

use super::{Compressor, DecompressionError, ENCODING_HEADER, compressors};

const BUFFER_SIZE: usize = 8 * 1024;

/// Information related to the decompression of a request or response
#[derive(Debug)]
pub struct Decompression {
    encoding: Option<String>,
}

impl Decompression {
    /// Create a `Decompression` structure
    pub fn new(encoding: Option<String>) -> Decompression {
        Decompression { encoding }
    }

    /// Create a `Decompression` structure from http headers
    pub fn from_headers(metadata: &http::HeaderMap) -> Decompression {
        let encoding = metadata
            .get(ENCODING_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        Decompression::new(encoding)
    }

    /// Get if the current encoding is the no-op "identity" one
    pub fn is_identity(&self) -> bool {
        match &self.encoding {
            Some(encoding) => encoding == compressors::IDENTITY,
            None => false,
        }
    }

    /// Find a compressor in the registry for the current encoding
    fn get_compressor(&self) -> Result<&Box<dyn Compressor>, DecompressionError> {
        match &self.encoding {
            None => {
                Ok(compressors::get(compressors::IDENTITY).expect("Identity is always present"))
            }
            Some(encoding) => match compressors::get(encoding) {
                Some(compressor) => Ok(compressor),
                None => Err(DecompressionError::NotFound {
                    requested: encoding.clone(),
                    known: compressors::names(),
                }),
            },
        }
    }

    /// Decompress `len` bytes from `in_buffer` into `out_buffer`
    pub fn decompress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> Result<(), DecompressionError> {
        let compressor = self.get_compressor()?;

        out_buffer
            .reserve(((compressor.estimate_decompressed_len(len) / BUFFER_SIZE) + 1) * BUFFER_SIZE);
        compressor.decompress(in_buffer, out_buffer, len)?;
        in_buffer.advance(len);

        debug!(
            "Decompressed {} bytes into {} bytes using {:?}",
            len,
            out_buffer.len(),
            self.encoding
        );
        Ok(())
    }
}

impl Default for Decompression {
    fn default() -> Self {
        Decompression { encoding: None }
    }
}
