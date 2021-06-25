use super::encode::BUFFER_SIZE;
use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};
use std::fmt::Write;

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct EnabledEncodings {
    gzip: bool,
}

impl EnabledEncodings {
    pub(crate) fn gzip(self) -> Self {
        Self { gzip: true }
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> http::HeaderValue {
        if self.gzip {
            http::HeaderValue::from_static("gzip,identity")
        } else {
            http::HeaderValue::from_static("identity")
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
pub(crate) enum Encoding {
    Gzip,
}

impl Encoding {
    pub(crate) fn from_accept_encoding_header(map: &http::HeaderMap) -> Option<Self> {
        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        split_by_comma(header_value_str).find_map(|value| match value {
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

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.trim().split(',').map(|s| s.trim())
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

    match encoding {
        Encoding::Gzip => {
            let mut gzip_decoder = GzEncoder::new(
                &in_buffer[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            let mut out_writer = out_buffer.writer();

            tokio::task::block_in_place(|| std::io::copy(&mut gzip_decoder, &mut out_writer))?;
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

            tokio::task::block_in_place(|| std::io::copy(&mut gzip_decoder, &mut out_writer))?;
        }
    }

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
        let encodings = EnabledEncodings { gzip: true };
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

        let encodings = EnabledEncodings { gzip: true };
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

        let encodings = EnabledEncodings { gzip: false };
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

        let encodings = EnabledEncodings { gzip: true };
        encodings.remove_disabled_encodings_from_accept_encoding(&mut map);

        assert_eq!(&map[ACCEPT_ENCODING_HEADER], "gzip");
    }
}
