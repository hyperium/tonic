use crate::arrow;
use prost::{bytes::Buf, Message};
use std::mem;
use tonic::{
    codec::{Codec, EncodeBuf, Encoder, ProstCodec},
    Status,
};

#[derive(Default)]
pub(crate) struct FlightDataCodec;

impl Codec for FlightDataCodec {
    type Encode = arrow::FlightData;
    type Decode = arrow::FlightData;
    type Encoder = FlightDataEncoder;
    type Decoder = <ProstCodec<(), arrow::FlightData> as Codec>::Decoder;

    fn encoder(&mut self) -> Self::Encoder {
        FlightDataEncoder::default()
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstCodec::<(), arrow::FlightData>::default().decoder()
    }
}

#[derive(Default)]
pub(crate) struct FlightDataEncoder;

impl Encoder for FlightDataEncoder {
    type Item = arrow::FlightData;
    type Error = Status;

    fn encode(&mut self, mut item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let body = mem::take(&mut item.data_body);
        item.encode_raw(buf);
        if body.has_remaining() {
            prost::encoding::encode_key(1000, prost::encoding::WireType::LengthDelimited, buf);
            prost::encoding::encode_varint(body.len() as u64, buf);
            buf.insert_slice(body);
        }
        Ok(())
    }
}
