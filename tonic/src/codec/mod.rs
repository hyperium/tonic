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

pub use self::decode::Streaming;
pub(crate) use self::encode::{encode_client, encode_server};
#[cfg(feature = "prost")]
#[cfg_attr(docsrs, doc(cfg(feature = "prost")))]
pub use self::prost::ProstCodec;
pub use tokio_codec::{Decoder, Encoder};

use crate::Status;

/// Trait that knows how to encode and decode gRPC messages.
pub trait Codec: Default {
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
