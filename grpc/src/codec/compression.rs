use bytes::{Buf, BufMut};
use std::io;

#[cfg(feature = "deflate")]
pub mod deflate;
#[cfg(feature = "gzip")]
pub mod gzip;
#[cfg(feature = "zstd")]
pub mod zstd;

pub mod registry;

pub use self::registry::get_codec;

/// A trait for identifying the encoding of a compression algorithm.
pub trait Encoding {
    /// The name of the compression algorithm, e.g., "gzip".
    const NAME: &'static str;
}

/// A trait for compressing and decompressing data.
pub trait Compressor: Send + Sync + 'static {
    /// Compress data from `source` into `destination`.
    fn compress(&self, source: &mut dyn Buf, destination: &mut dyn BufMut)
        -> Result<(), io::Error>;

    /// Decompress data from `source` into `destination`.
    fn decompress(
        &self,
        source: &mut dyn Buf,
        destination: &mut dyn BufMut,
    ) -> Result<(), io::Error>;
}
