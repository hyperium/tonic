use super::{Codec, DecodeBuf, Decoder, Encoder};
use crate::codec::EncodeBuf;
use crate::{Code, Status};
use bytes::{Buf, BufMut, Bytes};
use std::marker::PhantomData;

/// A [`Codec`] that implements `application/grpc+fbs` via the butte library..
#[derive(Debug, Clone)]
pub struct ButteCodec<T, U> {
    _pd: PhantomData<(T, U)>,
}

impl<T, U> Default for ButteCodec<T, U> {
    fn default() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<T, U> Codec for ButteCodec<T, U>
where
    T: Into<butte::Table<Bytes>> + Send + Sync + 'static,
    U: From<butte::Table<Bytes>> + Send + Sync + 'static,
{
    type Encode = T;
    type Decode = U;

    type Encoder = ButteEncoder<T>;
    type Decoder = ButteDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ButteEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        ButteDecoder(PhantomData)
    }
}

/// A [`Encoder`] that knows how to encode `T`.
#[derive(Debug, Clone, Default)]
pub struct ButteEncoder<T>(PhantomData<T>);

impl<T> Encoder for ButteEncoder<T>
where
    T: Into<butte::Table<Bytes>> + Send + Sync + 'static,
{
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let item = item.into().buf;
        buf.reserve(item.len());
        buf.put(item);
        Ok(())
    }
}

/// A [`Decoder`] that knows how to decode `U`.
#[derive(Debug, Clone, Default)]
pub struct ButteDecoder<U>(PhantomData<U>);

impl<U> Decoder for ButteDecoder<U>
where
    U: From<butte::Table<Bytes>> + Send + Sync + 'static,
{
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        if buf.has_remaining() {
            let bytes = buf.bytes();
            let bytes = Bytes::copy_from_slice(bytes);
            buf.advance(bytes.len());
            let res = U::from(<butte::Table<Bytes>>::get_root(bytes)?);
            Ok(Some(res))
        } else {
            Ok(None)
        }
    }
}

impl From<butte::Error> for crate::Status {
    fn from(e: butte::Error) -> Self {
        // Map Protobuf parse errors to an INTERNAL status code, as per
        // https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
        Status::new(Code::Internal, e.to_string())
    }
}
