use std::{collections::HashMap, io};

use super::bufwriter;
use bytes::{Buf, BytesMut};
use once_cell::sync::Lazy;

pub(crate) const IDENTITY: &str = "identity";

/// List of known compressors
static COMPRESSORS: Lazy<HashMap<String, Box<dyn Compressor>>> = Lazy::new(|| {
    let mut m = HashMap::new();

    let mut add = |compressor: Box<dyn Compressor>| {
        m.insert(compressor.name().to_string(), compressor);
    };

    add(Box::new(IdentityCompressor {}));

    #[cfg(feature = "gzip")]
    add(Box::new(super::gzip::GZipCompressor {}));

    m
});

/// Get a compressor from it's name
pub(crate) fn get(name: impl AsRef<str>) -> Option<&'static Box<dyn Compressor>> {
    COMPRESSORS.get(name.as_ref())
}

/// Get all the known compressors
pub(crate) fn names() -> Vec<String> {
    COMPRESSORS.keys().map(|n| n.clone()).collect()
}

/// A compressor implement compression and decompression of GRPC frames
pub(crate) trait Compressor: Sync + Send {
    /// Get the name of this compressor as present in http headers
    fn name(&self) -> &'static str;

    /// Decompress `len` bytes from `in_buffer` into `out_buffer`
    fn decompress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()>;

    /// Estimate the space necessary to decompress `compressed_len` bytes of compressed data
    fn estimate_decompressed_len(&self, compressed_len: usize) -> usize {
        compressed_len * 2
    }
}

/// The identity compressor doesn't compress
#[derive(Debug)]
struct IdentityCompressor {}

impl Compressor for IdentityCompressor {
    fn name(&self) -> &'static str {
        IDENTITY
    }

    fn decompress(
        &self,
        in_buffer: &mut BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()> {
        let mut in_reader = &in_buffer[0..len];
        let mut out_writer = bufwriter::new(out_buffer);

        std::io::copy(&mut in_reader, &mut out_writer)?;
        in_buffer.advance(len);

        Ok(())
    }

    fn estimate_decompressed_len(&self, compressed_len: usize) -> usize {
        compressed_len
    }
}
