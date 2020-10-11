use std::io;

use super::{Compressor, bufwriter};
use bytes::{Buf, BytesMut};
use flate2::read::GzDecoder;

/// Compress using GZIP
#[derive(Debug)]
pub(crate) struct GZipCompressor {}

impl Compressor for GZipCompressor {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn decompress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()> {
        let mut gzip_decoder = GzDecoder::new(&in_buffer[0..len]);
        let mut out_writer = bufwriter::new(out_buffer);

        std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        in_buffer.advance(len);

        Ok(())
    }
}
