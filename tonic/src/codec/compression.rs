use super::encode::BUFFER_SIZE;
use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AcceptEncoding {
    gzip: bool,
}

impl AcceptEncoding {
    pub(crate) fn gzip(self) -> Self {
        AcceptEncoding { gzip: true }
    }

    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        if self.gzip {
            http::HeaderValue::from_static("gzip,identity")
        } else {
            http::HeaderValue::from_static("identity")
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Encoding {
    Gzip,
}

impl Encoding {
    pub(crate) fn from_accept_encoding_header(map: &http::HeaderMap) -> Option<Self> {
        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        header_value_str
            .trim()
            .split(',')
            .map(|value| value.trim())
            .find_map(|value| match value {
                "gzip" => Some(Encoding::Gzip),
                _ => None,
            })
    }

    pub(crate) fn from_encoding_header(map: &http::HeaderMap) -> Option<Self> {
        let header_value = map.get(ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        match header_value_str {
            "gzip" => Some(Encoding::Gzip),
            _ => None,
        }
    }

    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        match self {
            Encoding::Gzip => http::HeaderValue::from_static("gzip"),
        }
    }
}

/// Compress `len` bytes from `in_buffer` into `out_buffer`.
pub(crate) fn compress(
    encoding: Encoding,
    in_buffer: &mut BytesMut,
    out_buffer: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buffer.reserve(capacity);

    // compressor.compress(in_buffer, out_buffer, len)?;

    match encoding {
        Encoding::Gzip => {
            let mut gzip_decoder = GzEncoder::new(
                &in_buffer[0..len],
                // TODO(david): what should compression level be?
                flate2::Compression::new(6),
            );
            let mut out_writer = out_buffer.writer();

            // TODO(david): use spawn blocking here
            std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        }
    }

    in_buffer.advance(len);

    Ok(())
}

pub(crate) fn decompress(
    encoding: Encoding,
    in_buffer: &mut BytesMut,
    out_buffer: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let estimate_decompressed_len = len * 2;
    let capacity = ((estimate_decompressed_len / BUFFER_SIZE) + 1) * BUFFER_SIZE;
    out_buffer.reserve(capacity);

    match encoding {
        Encoding::Gzip => {
            let mut gzip_decoder = GzDecoder::new(&in_buffer[0..len]);
            let mut out_writer = out_buffer.writer();

            // TODO(david): use spawn blocking here
            std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        }
    }

    in_buffer.advance(len);

    Ok(())
}
