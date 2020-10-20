use super::{
    compressors::{self, IDENTITY},
    errors::CompressionError,
    Compressor, ACCEPT_ENCODING_HEADER, ENCODING_HEADER,
};
use crate::metadata::MetadataMap;
use bytes::{Buf, BytesMut};
use http::HeaderValue;
use std::fmt::Debug;
use tracing::debug;

pub(crate) const BUFFER_SIZE: usize = 8 * 1024;

#[derive(Clone)]
pub(crate) struct Compression {
    compressor: Option<&'static Box<dyn Compressor>>,
}

impl Debug for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compression")
            .field(
                "compressor",
                &self.compressor.map(|c| c.name()).unwrap_or(IDENTITY),
            )
            .finish()
    }
}

impl Compression {
    /// Create an instance of compression that doesn't compress anything
    pub(crate) fn disabled() -> Compression {
        Compression { compressor: None }
    }

    /// Create an instance of compression from GRPC metadata
    pub(crate) fn response_from_metadata(request_metadata: &MetadataMap) -> Compression {
        // The following implementation is very conservative, and similar to the Golang GRPC implementation.
        // Instead of looking at 'grpc-accept-encoding' and potentially compressing the response with a different
        // compressor than the one used by the request it uses the same compressor
        let request_compressor = request_metadata
            .get(ENCODING_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(compressors::get);

        Compression {
            compressor: request_compressor,
        }
    }

    /// Get if compression is enabled
    pub(crate) fn is_enabled(&self) -> bool {
        self.compressor.is_some()
    }

    /// Decompress `len` bytes from `in_buffer` into `out_buffer`
    pub(crate) fn compress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> Result<(), CompressionError> {
        let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
        out_buffer.reserve(capacity);

        let compressor = self.compressor.ok_or(CompressionError::NoCompression)?;
        compressor.compress(in_buffer, out_buffer, len)?;
        in_buffer.advance(len);

        debug!(
            "Decompressed {} bytes into {} bytes using {:?}",
            len,
            out_buffer.len(),
            compressor.name()
        );

        Ok(())
    }

    /// Set the `grpc-encoding` header with the compressor name
    pub(crate) fn set_headers(&self, headers: &mut http::HeaderMap, set_accept_encoding: bool) {
        if set_accept_encoding {
            headers.insert(
                ACCEPT_ENCODING_HEADER,
                HeaderValue::from_str(&compressors::get_accept_encoding_header())
                    .expect("All encoding names should be ASCII"),
            );
        }

        match self.compressor {
            None => {}
            Some(compressor) => {
                headers.insert(ENCODING_HEADER, HeaderValue::from_static(compressor.name()));
            }
        }
    }
}
