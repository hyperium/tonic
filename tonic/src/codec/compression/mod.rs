mod bufwriter;
mod compression;
mod compressors;
mod decompression;
mod errors;

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";

#[cfg(feature = "gzip")]
mod gzip;

pub(crate) use self::compressors::Compressor;

#[doc(hidden)]
pub use self::decompression::Decompression;
pub(crate) use self::errors::DecompressionError;

pub(crate) use self::compression::Compression;
