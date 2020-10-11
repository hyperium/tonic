mod bufwriter;
mod compressors;
mod decompression;
mod errors;

#[cfg(feature = "gzip")]
mod gzip;

pub(crate) use self::compressors::Compressor;
#[doc(hidden)]
pub use self::decompression::Decompression;
pub(crate) use self::errors::DecompressionError;
