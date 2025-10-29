use crate::codec::compression::{Compressor, Encoding};
use bytes::{Buf, BufMut};
use flate2::{
    bufread::{GzDecoder, GzEncoder},
    Compression as FlateCompression,
};
use std::io;

/// A gzip compression implementation.
#[derive(Debug, Clone, Copy)]
pub struct Gzip {
    level: FlateCompression,
}

impl Gzip {
    /// Creates a new gzip compression implementation.
    pub fn new() -> Self {
        Self {
            level: FlateCompression::new(6),
        }
    }
}

impl Default for Gzip {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Gzip {
    fn compress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut encoder = GzEncoder::new(source.reader(), self.level);
        io::copy(&mut encoder, &mut destination.writer())?;
        Ok(())
    }

    fn decompress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut decoder = GzDecoder::new(source.reader());
        io::copy(&mut decoder, &mut destination.writer())?;
        Ok(())
    }
}

impl Encoding for Gzip {
    const NAME: &'static str = "gzip";
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn gzip_compress_decompress() {
        let compressor = Gzip::new();
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
