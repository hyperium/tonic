use bytes::{Buf, BufMut, Bytes};
use tonic::{
    codec::{Codec, Decoder, EncodeBuf, Encoder},
    Status,
};

/// An adapter for sending and receiving messages as bytes using tonic.
/// Coding/decoding is handled within gRPC.
/// TODO: Remove this when tonic allows access to bytes without requiring a
/// codec.
pub(crate) struct BytesCodec {}

impl Codec for BytesCodec {
    type Encode = Result<Bytes, Status>;
    type Decode = Bytes;
    type Encoder = BytesEncoder;
    type Decoder = BytesDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        BytesEncoder {}
    }

    fn decoder(&mut self) -> Self::Decoder {
        BytesDecoder {}
    }
}

pub struct BytesEncoder {}

impl Encoder for BytesEncoder {
    type Item = Result<Bytes, Status>;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        dst.put_slice(&item?);
        Ok(())
    }
}

#[derive(Debug)]
pub struct BytesDecoder {}

impl Decoder for BytesDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
