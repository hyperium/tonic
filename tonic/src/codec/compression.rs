use super::encode::BUFFER_SIZE;
use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};
use std::fmt::{self, Write};

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    pub(crate) gzip: bool,
}

impl EnabledCompressionEncodings {
    pub(crate) fn gzip(self) -> bool {
        self.gzip
    }

    /// Enable `gzip` compression.
    pub fn enable_gzip(&mut self) {
        self.gzip = true;
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        if self.gzip {
            Some(http::HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }

    /// Find the `grpc-accept-encoding` header and remove the encoding values that aren't enabled.
    ///
    /// For example a header value like `gzip,brotli,identity` where only `gzip` is enabled will
    /// become `gzip`.
    ///
    /// This is used to remove disabled encodings from incoming requests in the server before they
    /// each the actual `server::Grpc` service implementation. It is not possible to configure
    /// `server::Grpc` so the configuration must be done at the `Server` level.
    pub(crate) fn remove_disabled_encodings_from_accept_encoding(self, map: &mut http::HeaderMap) {
        let accept_encoding = if let Some(accept_encoding) = map.remove(ACCEPT_ENCODING_HEADER) {
            accept_encoding
        } else {
            return;
        };

        let accept_encoding_str = if let Ok(accept_encoding) = accept_encoding.to_str() {
            accept_encoding
        } else {
            map.insert(
                http::header::HeaderName::from_static(ACCEPT_ENCODING_HEADER),
                accept_encoding,
            );
            return;
        };

        // first check if we need to make changes to avoid allocating
        let contains_disabled_encodings =
            split_by_comma(accept_encoding_str).any(|encoding| match encoding {
                "gzip" => !self.gzip,
                _ => true,
            });

        if !contains_disabled_encodings {
            // no changes necessary, put the original value back
            map.insert(
                http::header::HeaderName::from_static(ACCEPT_ENCODING_HEADER),
                accept_encoding,
            );
            return;
        }

        // can be simplified when `Iterator::intersperse` is stable
        let enabled_encodings =
            split_by_comma(accept_encoding_str).filter_map(|encoding| match encoding {
                "gzip" if self.gzip => Some("gzip"),
                _ => None,
            });

        let mut new_value = String::new();
        let mut is_first = true;

        for encoding in enabled_encodings {
            if is_first {
                let _ = write!(new_value, "{}", encoding);
            } else {
                let _ = write!(new_value, ",{}", encoding);
            };
            is_first = false;
        }

        if !new_value.is_empty() {
            map.insert(
                http::header::HeaderName::from_static(ACCEPT_ENCODING_HEADER),
                new_value.parse().unwrap(),
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
#[doc(hidden)]
pub enum CompressionEncoding {
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

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use http::header::{HeaderMap, HeaderName};

    #[test]
    fn remove_disabled_encodings_empty_map() {
        let mut map = HeaderMap::new();
        let encodings = EnabledCompressionEncodings { gzip: true };
        encodings.remove_disabled_encodings_from_accept_encoding(&mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn remove_disabled_encodings_single_supported() {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static(ACCEPT_ENCODING_HEADER),
            "gzip".parse().unwrap(),
        );

        let encodings = EnabledCompressionEncodings { gzip: true };
        encodings.remove_disabled_encodings_from_accept_encoding(&mut map);

        assert_eq!(&map[ACCEPT_ENCODING_HEADER], "gzip");
    }

    #[test]
    fn remove_disabled_encodings_single_unsupported() {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static(ACCEPT_ENCODING_HEADER),
            "gzip".parse().unwrap(),
        );

        let encodings = EnabledCompressionEncodings { gzip: false };
        encodings.remove_disabled_encodings_from_accept_encoding(&mut map);

        assert!(map.get(ACCEPT_ENCODING_HEADER).is_none());
    }

    #[test]
    fn remove_disabled_encodings_multiple_supported() {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static(ACCEPT_ENCODING_HEADER),
            "foo,gzip,identity".parse().unwrap(),
        );

        let encodings = EnabledCompressionEncodings { gzip: true };
        encodings.remove_disabled_encodings_from_accept_encoding(&mut map);

        assert_eq!(&map[ACCEPT_ENCODING_HEADER], "gzip");
    }
}
