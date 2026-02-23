use crate::{metadata::MetadataValue, Status};
use bytes::{Buf, BufMut, BytesMut};
#[cfg(feature = "gzip")]
use flate2::read::{GzDecoder, GzEncoder};
#[cfg(feature = "deflate")]
use flate2::read::{ZlibDecoder, ZlibEncoder};
#[cfg(any(feature = "snappy", feature = "lz4"))]
use std::io::Write;
use std::{borrow::Cow, fmt};
#[cfg(feature = "zstd")]
use zstd::stream::read::{Decoder, Encoder};

pub(crate) const ENCODING_HEADER: &str = "grpc-encoding";
pub(crate) const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings are enabled on a server or channel.
///
/// Represents an ordered list of compression encodings that are enabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnabledCompressionEncodings {
    inner: [Option<CompressionEncoding>; 5],
}

impl EnabledCompressionEncodings {
    /// Enable a [`CompressionEncoding`].
    ///
    /// Adds the new encoding to the end of the encoding list.
    pub fn enable(&mut self, encoding: CompressionEncoding) {
        for e in self.inner.iter_mut() {
            match e {
                Some(e) if *e == encoding => return,
                None => {
                    *e = Some(encoding);
                    return;
                }
                _ => continue,
            }
        }
    }

    /// Remove the last [`CompressionEncoding`].
    pub fn pop(&mut self) -> Option<CompressionEncoding> {
        self.inner
            .iter_mut()
            .rev()
            .find(|entry| entry.is_some())?
            .take()
    }

    pub(crate) fn into_accept_encoding_header_value(self) -> Option<http::HeaderValue> {
        let mut value = BytesMut::new();
        for encoding in self.inner.into_iter().flatten() {
            value.put_slice(encoding.as_str().as_bytes());
            value.put_u8(b',');
        }

        if value.is_empty() {
            return None;
        }

        value.put_slice(b"identity");
        Some(http::HeaderValue::from_maybe_shared(value).unwrap())
    }

    /// Check if a [`CompressionEncoding`] is enabled.
    pub fn is_enabled(&self, encoding: CompressionEncoding) -> bool {
        self.inner.contains(&Some(encoding))
    }

    /// Check if any [`CompressionEncoding`]s are enabled.
    pub fn is_empty(&self) -> bool {
        self.inner.iter().all(|e| e.is_none())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompressionSettings {
    pub(crate) encoding: CompressionEncoding,
    /// buffer_growth_interval controls memory growth for internal buffers to balance resizing cost against memory waste.
    /// The default buffer growth interval is 8 kilobytes.
    pub(crate) buffer_growth_interval: usize,
}

/// The compression encodings Tonic supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompressionEncoding {
    #[allow(missing_docs)]
    #[cfg(feature = "gzip")]
    Gzip,
    #[allow(missing_docs)]
    #[cfg(feature = "deflate")]
    Deflate,
    #[allow(missing_docs)]
    #[cfg(feature = "zstd")]
    Zstd,
    #[allow(missing_docs)]
    #[cfg(feature = "lz4")]
    Lz4,
    #[allow(missing_docs)]
    #[cfg(feature = "snappy")]
    Snappy,
}

impl CompressionEncoding {
    pub(crate) const ENCODINGS: &'static [CompressionEncoding] = &[
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip,
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate,
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd,
        #[cfg(feature = "lz4")]
        CompressionEncoding::Lz4,
        #[cfg(feature = "snappy")]
        CompressionEncoding::Snappy,
    ];

    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub(crate) fn from_accept_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Option<Self> {
        if enabled_encodings.is_empty() {
            return None;
        }

        let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
        let header_value_str = header_value.to_str().ok()?;

        split_by_comma(header_value_str).find_map(|value| match value {
            #[cfg(feature = "gzip")]
            "gzip" => Some(CompressionEncoding::Gzip),
            #[cfg(feature = "deflate")]
            "deflate" => Some(CompressionEncoding::Deflate),
            #[cfg(feature = "zstd")]
            "zstd" => Some(CompressionEncoding::Zstd),
            #[cfg(feature = "lz4")]
            "lz4" => Some(CompressionEncoding::Lz4),
            #[cfg(feature = "snappy")]
            "snappy" => Some(CompressionEncoding::Snappy),
            _ => None,
        })
    }

    /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't supported.
    pub(crate) fn from_encoding_header(
        map: &http::HeaderMap,
        enabled_encodings: EnabledCompressionEncodings,
    ) -> Result<Option<Self>, Status> {
        let Some(header_value) = map.get(ENCODING_HEADER) else {
            return Ok(None);
        };

        match header_value.as_bytes() {
            #[cfg(feature = "gzip")]
            b"gzip" if enabled_encodings.is_enabled(CompressionEncoding::Gzip) => {
                Ok(Some(CompressionEncoding::Gzip))
            }
            #[cfg(feature = "deflate")]
            b"deflate" if enabled_encodings.is_enabled(CompressionEncoding::Deflate) => {
                Ok(Some(CompressionEncoding::Deflate))
            }
            #[cfg(feature = "zstd")]
            b"zstd" if enabled_encodings.is_enabled(CompressionEncoding::Zstd) => {
                Ok(Some(CompressionEncoding::Zstd))
            }
            #[cfg(feature = "lz4")]
            b"lz4" if enabled_encodings.is_enabled(CompressionEncoding::Lz4) => {
                Ok(Some(CompressionEncoding::Lz4))
            }
            #[cfg(feature = "snappy")]
            b"snappy" if enabled_encodings.is_enabled(CompressionEncoding::Snappy) => {
                Ok(Some(CompressionEncoding::Snappy))
            }
            b"identity" => Ok(None),
            other => {
                let other = match std::str::from_utf8(other) {
                    Ok(s) => Cow::Borrowed(s),
                    Err(_) => Cow::Owned(format!("{other:?}")),
                };

                let mut status = Status::unimplemented(format!(
                    "Content is compressed with `{other}` which isn't supported"
                ));

                let header_value = enabled_encodings
                    .into_accept_encoding_header_value()
                    .map(MetadataValue::unchecked_from_header_value)
                    .unwrap_or_else(|| MetadataValue::from_static("identity"));
                status
                    .metadata_mut()
                    .insert(ACCEPT_ENCODING_HEADER, header_value);

                Err(status)
            }
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip => "gzip",
            #[cfg(feature = "deflate")]
            CompressionEncoding::Deflate => "deflate",
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd => "zstd",
            #[cfg(feature = "lz4")]
            CompressionEncoding::Lz4 => "lz4",
            #[cfg(feature = "snappy")]
            CompressionEncoding::Snappy => "snappy",
        }
    }

    #[cfg(any(
        feature = "gzip",
        feature = "deflate",
        feature = "zstd",
        feature = "lz4",
        feature = "snappy"
    ))]
    pub(crate) fn into_header_value(self) -> http::HeaderValue {
        http::HeaderValue::from_static(self.as_str())
    }
}

impl fmt::Display for CompressionEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn split_by_comma(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(|s| s.trim())
}

/// Compress `len` bytes from `decompressed_buf` into `out_buf`.
/// buffer_size_increment is a hint to control the growth of out_buf versus the cost of resizing it.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn compress(
    settings: CompressionSettings,
    decompressed_buf: &mut BytesMut,
    out_buf: &mut BytesMut,
    len: usize,
) -> Result<(), std::io::Error> {
    let buffer_growth_interval = settings.buffer_growth_interval;
    let capacity = ((len / buffer_growth_interval) + 1) * buffer_growth_interval;
    out_buf.reserve(capacity);

    #[cfg(any(
        feature = "gzip",
        feature = "deflate",
        feature = "zstd",
        feature = "lz4",
        feature = "snappy"
    ))]
    let mut out_writer = out_buf.writer();

    match settings.encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            let mut gzip_encoder = GzEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            std::io::copy(&mut gzip_encoder, &mut out_writer)?;
        }
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate => {
            let mut deflate_encoder = ZlibEncoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                flate2::Compression::new(6),
            );
            std::io::copy(&mut deflate_encoder, &mut out_writer)?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd => {
            let mut zstd_encoder = Encoder::new(
                &decompressed_buf[0..len],
                // FIXME: support customizing the compression level
                zstd::DEFAULT_COMPRESSION_LEVEL,
            )?;
            std::io::copy(&mut zstd_encoder, &mut out_writer)?;
        }
        #[cfg(feature = "lz4")]
        CompressionEncoding::Lz4 => {
            {
                let lz4_encoder = lz4_flex::frame::FrameEncoder::new(&mut out_writer);
                let mut auto_finish_encoder = lz4_encoder.auto_finish();
                auto_finish_encoder.write_all(&decompressed_buf[0..len])?;
                // lz4_encoder is dropped here, flushing the final frame
            }
        }
        #[cfg(feature = "snappy")]
        CompressionEncoding::Snappy => {
            {
                let mut snappy_encoder = snap::write::FrameEncoder::new(&mut out_writer);
                snappy_encoder.write_all(&decompressed_buf[0..len])?;
                // snappy_encoder is dropped here, flushing the final frame
            }
        }
    }

    decompressed_buf.advance(len);

    Ok(())
}

/// Decompress `len` bytes from `compressed_buf` into `out_buf`.
#[allow(unused_variables, unreachable_code)]
pub(crate) fn decompress(
    settings: CompressionSettings,
    compressed_buf: &mut BytesMut,
    mut out_buf: bytes::buf::Limit<&mut BytesMut>,
    len: usize,
) -> Result<(), std::io::Error> {
    let buffer_growth_interval = settings.buffer_growth_interval;
    let estimate_decompressed_len = len * 2;
    let capacity = std::cmp::min(
        bytes::buf::Limit::limit(&out_buf),
        ((estimate_decompressed_len / buffer_growth_interval) + 1) * buffer_growth_interval,
    );

    #[cfg(any(
        feature = "gzip",
        feature = "deflate",
        feature = "zstd",
        feature = "lz4",
        feature = "snappy"
    ))]
    let mut out_writer = out_buf.writer();

    match settings.encoding {
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip => {
            let mut gzip_decoder = GzDecoder::new(&compressed_buf[0..len]);
            std::io::copy(&mut gzip_decoder, &mut out_writer)?;
        }
        #[cfg(feature = "deflate")]
        CompressionEncoding::Deflate => {
            let mut deflate_decoder = ZlibDecoder::new(&compressed_buf[0..len]);
            std::io::copy(&mut deflate_decoder, &mut out_writer)?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd => {
            let mut zstd_decoder = Decoder::new(&compressed_buf[0..len])?;
            std::io::copy(&mut zstd_decoder, &mut out_writer)?;
        }
        #[cfg(feature = "lz4")]
        CompressionEncoding::Lz4 => {
            let mut lz4_decoder = lz4_flex::frame::FrameDecoder::new(&compressed_buf[0..len]);
            std::io::copy(&mut lz4_decoder, &mut out_writer)?;
        }
        #[cfg(feature = "snappy")]
        CompressionEncoding::Snappy => {
            let mut snappy_decoder = snap::read::FrameDecoder::new(&compressed_buf[0..len]);
            std::io::copy(&mut snappy_decoder, &mut out_writer)?;
        }
    }

    compressed_buf.advance(len);

    Ok(())
}

/// Controls compression behavior for individual messages within a stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SingleMessageCompressionOverride {
    /// Inherit whatever compression is already configured. If the stream is compressed this
    /// message will also be configured.
    ///
    /// This is the default.
    #[default]
    Inherit,
    /// Don't compress this message, even if compression is enabled on the stream.
    Disable,
}

#[cfg(test)]
mod tests {
    #[cfg(any(
        feature = "gzip",
        feature = "deflate",
        feature = "zstd",
        feature = "lz4",
        feature = "snappy"
    ))]
    use http::HeaderValue;

    use super::*;

    #[test]
    fn convert_none_into_header_value() {
        let encodings = EnabledCompressionEncodings::default();

        assert!(encodings.into_accept_encoding_header_value().is_none());
    }

    #[test]
    #[cfg(feature = "gzip")]
    fn convert_gzip_into_header_value() {
        const GZIP: HeaderValue = HeaderValue::from_static("gzip,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Gzip), None, None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), GZIP);

        let encodings = EnabledCompressionEncodings {
            inner: [None, Some(CompressionEncoding::Gzip), None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), GZIP);
    }

    #[test]
    #[cfg(feature = "zstd")]
    fn convert_zstd_into_header_value() {
        const ZSTD: HeaderValue = HeaderValue::from_static("zstd,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Zstd), None, None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), ZSTD);

        let encodings = EnabledCompressionEncodings {
            inner: [None, Some(CompressionEncoding::Zstd), None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), ZSTD);
    }

    #[test]
    #[cfg(feature = "snappy")]
    fn convert_snappy_into_header_value() {
        const SNAPPY: HeaderValue = HeaderValue::from_static("snappy,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Snappy), None, None, None, None],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            SNAPPY
        );

        let encodings = EnabledCompressionEncodings {
            inner: [None, Some(CompressionEncoding::Snappy), None, None, None],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            SNAPPY
        );
    }

    #[test]
    #[cfg(feature = "lz4")]
    fn convert_lz4_into_header_value() {
        const LZ4: HeaderValue = HeaderValue::from_static("lz4,identity");

        let encodings = EnabledCompressionEncodings {
            inner: [Some(CompressionEncoding::Lz4), None, None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), LZ4);

        let encodings = EnabledCompressionEncodings {
            inner: [None, Some(CompressionEncoding::Lz4), None, None, None],
        };

        assert_eq!(encodings.into_accept_encoding_header_value().unwrap(), LZ4);
    }

    #[test]
    #[cfg(all(
        feature = "gzip",
        feature = "deflate",
        feature = "zstd",
        feature = "snappy",
        feature = "lz4"
    ))]
    fn convert_gzip_zstd_snappy_and_lz4_into_header_value() {
        let encodings = EnabledCompressionEncodings {
            inner: [
                Some(CompressionEncoding::Gzip),
                Some(CompressionEncoding::Deflate),
                Some(CompressionEncoding::Zstd),
                Some(CompressionEncoding::Snappy),
                Some(CompressionEncoding::Lz4),
            ],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            HeaderValue::from_static("gzip,deflate,zstd,snappy,lz4,identity"),
        );

        let encodings = EnabledCompressionEncodings {
            inner: [
                Some(CompressionEncoding::Zstd),
                Some(CompressionEncoding::Deflate),
                Some(CompressionEncoding::Gzip),
                Some(CompressionEncoding::Snappy),
                Some(CompressionEncoding::Lz4),
            ],
        };

        assert_eq!(
            encodings.into_accept_encoding_header_value().unwrap(),
            HeaderValue::from_static("zstd,deflate,gzip,snappy,lz4,identity"),
        );
    }
}
