use crate::codec::compression::{Compressor, Encoding};
use bytes::{Buf, BufMut};
use flate2::{
    bufread::{ZlibDecoder, ZlibEncoder},
    Compression as FlateCompression,
};
use std::io;

/// A deflate compression implementation.
#[derive(Debug, Clone, Copy)]
pub struct Deflate {
    level: FlateCompression,
}

impl Deflate {
    /// Creates a new deflate compression implementation.
    pub fn new() -> Self {
        Self {
            level: FlateCompression::new(6),
        }
    }
}

impl Default for Deflate {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Deflate {
    fn compress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut encoder = ZlibEncoder::new(source.reader(), self.level);
        io::copy(&mut encoder, &mut destination.writer())?;
        Ok(())
    }

    fn decompress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut decoder = ZlibDecoder::new(source.reader());
        io::copy(&mut decoder, &mut destination.writer())?;
        Ok(())
    }
}

impl Encoding for Deflate {
    const NAME: &'static str = "deflate";
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn deflate_compress_decompress() {
        let compressor = Deflate::new();
        let data = Bytes::from_static(b"hello world");
        let mut compressed = Vec::new();
        compressor
            .compress(&mut data.clone(), &mut compressed)
            .unwrap();
        let mut decompressed = Vec::new();
        compressor
            .decompress(&mut compressed.as_slice(), &mut decompressed)
            .unwrap();
        assert_eq!(data, decompressed.as_slice());
    }
}
