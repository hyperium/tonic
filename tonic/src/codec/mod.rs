//! Generic encoding and decoding.
//!
//! This module contains the generic `Codec`, `Encoder` and `Decoder` traits
//! and a protobuf codec based on prost.

mod buffer;
mod decode;
mod encode;
#[cfg(feature = "data-prost")]
mod prost;

#[cfg(all(test, feature = "data-prost"))]
mod prost_tests;

use std::io;

pub use self::decode::Streaming;
pub(crate) use self::encode::{encode_client, encode_server};
#[cfg(feature = "data-prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "data-prost")))]
pub use self::prost::ProstCodec;
use crate::Status;
pub use buffer::{DecodeBuf, EncodeBuf};

/// Trait that knows how to encode and decode gRPC messages.
pub trait Codec: Default {
    /// The encodable message.
    type Encode: Send + 'static;
    /// The decodable message.
    type Decode: Send + 'static;

    /// The encoder that can encode a message.
    type Encoder: Encoder<Item = Self::Encode, Error = Status> + Send + Sync + 'static;
    /// The encoder that can decode a message.
    type Decoder: Decoder<Item = Self::Decode, Error = Status> + Send + Sync + 'static;

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
}
