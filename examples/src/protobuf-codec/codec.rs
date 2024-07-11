use bytes::{Buf, BufMut};
use protobuf::Message;
use std::marker::PhantomData;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::Status;

pub struct ProtobufCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for ProtobufCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for ProtobufCodec<T, U>
where
    T: Message,
    U: Message,
{
    type Encode = T;
    type Decode = U;
    type Encoder = ProtobufEncoder<T>;
    type Decoder = ProtobufDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ProtobufEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProtobufDecoder(PhantomData)
    }
}

pub struct ProtobufEncoder<T>(PhantomData<T>);

impl<T> Encoder for ProtobufEncoder<T>
where
    T: Message,
{
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        Ok(item
            .write_to_writer(&mut dst.writer())
            .map_err(|_| Status::internal("failed to encode"))?)
    }
}

pub struct ProtobufDecoder<U>(PhantomData<U>);

impl<U> Decoder for ProtobufDecoder<U>
where
    U: Message,
{
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        Ok(Some(
            U::parse_from_reader(&mut src.reader())
                .map_err(|_| Status::invalid_argument("bad request"))?,
        ))
    }
}
