mod decode;
mod encode;
mod prost;

pub use self::decode::{decode_empty, decode_request, decode_response, Streaming};
pub use self::encode::encode;
pub use self::prost::ProstCodec;

use crate::Status;
use tokio_codec::{Decoder, Encoder};

pub trait Codec {
    type Encode;
    type Decode;

    type Encoder: Encoder<Item = Self::Encode, Error = Status>;
    type Decoder: Decoder<Item = Self::Decode, Error = Status>;

    const CONTENT_TYPE: &'static str;

    fn encoder(&mut self) -> Self::Encoder;
    fn decoder(&mut self) -> Self::Decoder;
}
