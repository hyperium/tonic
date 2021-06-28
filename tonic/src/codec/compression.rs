use super::encode::BUFFER_SIZE;
use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};
use std::fmt;

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    pub(crate) gzip: bool,
}

impl EnabledCompressionEncodings {
    /// Check if `gzip` compression is enabled.
    pub fn gzip(self) -> bool {
        self.gzip
    }

    /// Enable `gzip` compression.
    pub fn enable_gzip(&mut self) {
        self.gzip = true;
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        let Self { gzip } = self;
        if gzip {
            Some(http::HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }
}

/// The compression encodings Tonic supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionEncoding {
    #[allow(missing_docs)]
    Gzip,
}

impl CompressionEncoding {
    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub(crate) fn from_accept_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Option<Self> {
        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        split_by_comma(header_value_str).find_map(|value| match value {
            "gzip" if enabled_encodings.gzip() => Some(CompressionEncoding::Gzip),
            _ => None,
        })
    }

    pub(crate) fn from_encoding_header(map: &http::HeaderMap) -> Option<Self> {
        let header_value = map.get(ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        match header_value_str {
            "gzip" => Some(CompressionEncoding::Gzip),
            _ => None,
        }
    }

    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        match self {
            CompressionEncoding::Gzip => http::HeaderValue::from_static("gzip"),
        }
    }
}

impl fmt::Display for CompressionEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompressionEncoding::Gzip => write!(f, "gzip"),
        }
    }
}

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.trim().split(',').map(|s| s.trim())
}

/// Compress `len` bytes from `in_buffer` into `out_buffer`.
pub(crate) fn compress<B>(
    encoding: CompressionEncoding,
    in_buffer: &mut B,
    out_buffer: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error>
where
    B: AsRef<[u8]> + bytes::Buf,
{
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buffer.reserve(capacity);

    match encoding {
        CompressionEncoding::Gzip => {
            let mut gzip_decoder = GzEncoder::new(
                &in_buffer.as_ref()[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            let mut out_writer = out_buffer.writer();

            tokio::task::block_in_place(|| std::io::copy(&mut gzip_decoder, &mut out_writer))?;
        }
    }

    // TODO(david): is this necessary? test sending multiple requests and
    // responses on the same channel
    in_buffer.advance(len);

    Ok(())
}

pub(crate) fn decompress(
    encoding: CompressionEncoding,
    in_buffer: &mut BytesMut,
    out_buffer: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let estimate_decompressed_len = len * 2;
    let capacity = ((estimate_decompressed_len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buffer.reserve(capacity);

    match encoding {
        CompressionEncoding::Gzip => {
            let mut gzip_decoder = GzDecoder::new(&in_buffer[0..len]);
            let mut out_writer = out_buffer.writer();

            tokio::task::block_in_place(|| std::io::copy(&mut gzip_decoder, &mut out_writer))?;
        }
    }

    // TODO(david): is this necessary? test sending multiple requests and
    // responses on the same channel
    in_buffer.advance(len);

    Ok(())
}
