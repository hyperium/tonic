use crate::codec::compression::{Compressor, Encoding};
use bytes::{Buf, BufMut};
use std::io;
use zstd::stream::{read::Decoder, read::Encoder};

/// A zstd compression implementation.
#[derive(Debug, Clone, Copy)]
pub struct Zstd {
    level: i32,
}

impl Zstd {
    /// Creates a new zstd compression implementation with default compression level.
    pub fn new() -> Self {
        Self::with_level(zstd::DEFAULT_COMPRESSION_LEVEL)
    }

    /// Creates a new zstd compression implementation with a specific compression level.
    pub fn with_level(level: i32) -> Self {
        Self { level }
    }
}

impl Default for Zstd {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Zstd {
    fn compress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut encoder = Encoder::new(source.reader(), self.level)?;
        io::copy(&mut encoder, &mut destination.writer())?;
        Ok(())
    }

    fn decompress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error> {
        let mut decoder = Decoder::new(source.reader())?;
        io::copy(&mut decoder, &mut destination.writer())?;
        Ok(())
    }
}

impl Encoding for Zstd {
    const NAME: &'static str = "zstd";
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn zstd_compress_decompress() {
        let compressor = Zstd::new();
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
