use std::io;

use super::{bufwriter, Compressor};
use bytes::BytesMut;
use flate2::read::{GzDecoder, GzEncoder};

/// Compress using GZIP
#[derive(Debug)]
pub(crate) struct GZipCompressor {
    compression_level: flate2::Compression
}

impl GZipCompressor {
    fn new(compression_level: flate2::Compression) -> GZipCompressor {
        GZipCompressor { compression_level }
    }
}

impl Default for GZipCompressor {
    fn default() -> Self {
        Self::new(flate2::Compression::new(6))
    }
}

impl Compressor for GZipCompressor {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn decompress(
        &self,
        in_buffer: &BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()> {
        let mut gzip_decoder = GzDecoder::new(&in_buffer[0..len]);
        let mut out_writer = bufwriter::new(out_buffer);

        std::io::copy(&mut gzip_decoder, &mut out_writer)?;

        Ok(())
    }

    fn compress(
        &self,
        in_buffer: &BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()> {
        let mut gzip_decoder = GzEncoder::new(&in_buffer[0..len], self.compression_level);
        let mut out_writer = bufwriter::new(out_buffer);

        std::io::copy(&mut gzip_decoder, &mut out_writer)?;

        Ok(())
    }
}
