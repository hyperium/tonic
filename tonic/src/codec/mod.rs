//! Generic encoding and decoding.
//!
//! This module contains the generic `Codec` trait and a protobuf codec
//! based on prost.

mod decode;
mod encode;
#[cfg(feature = "prost")]
mod prost;

#[cfg(test)]
mod tests;

use std::io;

use bytes::{Buf, BytesMut};

pub use self::decode::Streaming;
pub(crate) use self::encode::{encode_client, encode_server};
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use self::prost::ProstCodec;
use crate::Status;

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

/// Decoding of frames via buffers.
pub trait Decoder {
    /// The type of decoded frames.
    type Item;

    /// The type of unrecoverable frame decoding errors.
    type Error: From<io::Error>;

    /// Attempts to decode a frame from the provided buffer of bytes.
    fn decode(&mut self, src: &mut dyn Buf) -> Result<Option<Self::Item>, Self::Error>;
}

/// Trait of helper objects to write out messages as bytes.
pub trait Encoder {
    /// The type of items consumed by the `Encoder`
    type Item;

    /// The type of encoding errors.
    ///
    /// The type of unrecoverable frame encoding errors.
    type Error: From<io::Error>;

    /// Encodes a frame into the buffer provided.
    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error>;
}
