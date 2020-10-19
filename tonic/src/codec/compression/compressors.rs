use bytes::BytesMut;
use once_cell::sync::Lazy;
use std::{collections::HashMap, io};

pub(crate) const IDENTITY: &str = "identity";

/// List of known compressors
static COMPRESSORS: Lazy<HashMap<String, Box<dyn Compressor>>> = Lazy::new(|| {
    #[cfg(feature = "gzip")]
    {
        let mut m = HashMap::new();

        let mut add = |compressor: Box<dyn Compressor>| {
            m.insert(compressor.name().to_string(), compressor);
        };

        add(Box::new(super::gzip::GZipCompressor::default()));

        m
    }

    #[cfg(not(feature = "gzip"))]
    HashMap::new()
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
        in_buffer: &BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()>;

    /// Compress `len` bytes from `in_buffer` into `out_buffer`
    fn compress(
        &self,
        in_buffer: &BytesMut,
        out_buffer: &mut BytesMut,
        len: usize,
    ) -> io::Result<()>;

    /// Estimate the space necessary to decompress `compressed_len` bytes of compressed data
    fn estimate_decompressed_len(&self, compressed_len: usize) -> usize {
        compressed_len * 2
    }
}

pub(crate) fn get_accept_encoding_header() -> String {
    COMPRESSORS
        .keys()
        .map(|s| &**s)
        .collect::<Vec<_>>()
        .join(",")
}
