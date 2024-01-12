//! Generic encoding and decoding.
//!
//! This module contains the generic `Codec`, `Encoder` and `Decoder` traits
//! and a protobuf codec based on prost.

mod buffer;
pub(crate) mod compression;
mod decode;
mod encode;
#[cfg(feature = "prost")]
mod prost;

use crate::Status;
use std::io;

pub(crate) use self::encode::{encode_client, encode_server};

pub use self::buffer::{DecodeBuf, EncodeBuf};
pub use self::compression::{CompressionEncoding, EnabledCompressionEncodings};
pub use self::decode::Streaming;
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use self::prost::ProstCodec;
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use self::prost::ProstDecoder;
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use self::prost::ProstEncoder;

/// Unless overridden, this is the buffer size used for encoding requests.
/// This is spent per-rpc, so you may wish to adjust it. The default is
/// pretty good for most uses, but if you have a ton of concurrent rpcs
/// you may find it too expensive.
const DEFAULT_CODEC_BUFFER_SIZE: usize = 8 * 1024;
const DEFAULT_YIELD_THRESHOLD: usize = 32 * 1024;

/// Settings for how tonic allocates and grows buffers.
#[derive(Clone, Copy, Debug)]
pub struct BufferSettings {
    /// Initial buffer size, and the growth unit for cases where the size
    /// is larger than the buffer's current capacity. Defaults to 8 KiB.
    ///
    /// Notably, this is eagerly allocated per streaming rpc.
    pub buffer_size: usize,

    /// Soft maximum size for returning a stream's ready contents in a batch,
    /// rather than one-by-one. Defaults to 32 KiB.
    pub yield_threshold: usize,
}
impl Default for BufferSettings {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_CODEC_BUFFER_SIZE,
            yield_threshold: DEFAULT_YIELD_THRESHOLD,
        }
    }
}

// 5 bytes
const HEADER_SIZE: usize =
    // compression flag
    std::mem::size_of::<u8>() +
    // data length
    std::mem::size_of::<u32>();

// The default maximum uncompressed size in bytes for a message. Defaults to 4MB.
const DEFAULT_MAX_RECV_MESSAGE_SIZE: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_SEND_MESSAGE_SIZE: usize = usize::MAX;

/// Trait that knows how to encode and decode gRPC messages.
pub trait Codec {
    /// The encodable message.
    type Encode: Send + 'static;
    /// The decodable message.
    type Decode: Send + 'static;

    /// The encoder that can encode a message.
    type Encoder: Encoder<Item = Self::Encode, Error = Status> + Send + 'static;
    /// The encoder that can decode a message.
    type Decoder: Decoder<Item = Self::Decode, Error = Status> + Send + 'static;

    /// Fetch the encoder.
    fn encoder(&mut self) -> Self::Encoder;
    /// Fetch the decoder.
    fn decoder(&mut self) -> Self::Decoder;
}

/// Encodes gRPC message types
pub trait Encoder {
    /// The type that is encoded.
    type Item;

    /// The type of encoding errors.
    ///
    /// The type of unrecoverable frame encoding errors.
    type Error: From<io::Error>;

    /// Encodes a message into the provided buffer.
    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error>;

    /// Controls how tonic creates and expands encode buffers.
    fn buffer_settings(&self) -> BufferSettings;
}

/// Decodes gRPC message types
pub trait Decoder {
    /// The type that is decoded.
    type Item;

    /// The type of unrecoverable frame decoding errors.
    type Error: From<io::Error>;

    /// Decode a message from the buffer.
    ///
    /// The buffer will contain exactly the bytes of a full message. There
    /// is no need to get the length from the bytes, gRPC framing is handled
    /// for you.
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error>;

    /// Controls how tonic creates and expands decode buffers.
    fn buffer_settings(&self) -> BufferSettings;
}
