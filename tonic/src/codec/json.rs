use std::marker::PhantomData;
use crate::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use crate::Status;
use bytes::{Buf, BufMut};

extern crate serde;
extern crate serde_derive;

struct JsonEncoder<T>(PhantomData<T>);

impl<T: serde::Serialize> Encoder for JsonEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let s = serde_json::to_string(&item).expect("Message only errors if not enough space");
        buf.put(s.as_bytes());

        Ok(())
    }
}

pub struct JsonDecoder<U>(PhantomData<U>);

impl<U: for<'a> serde::Deserialize<'a>> Decoder for JsonDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let item = serde_json::from_reader(buf.reader()).map_err(from_decode_error)?;

        Ok(item)
    }
}

fn from_decode_error(error: serde_json::Error) -> crate::Status {
    // Map Protobuf parse errors to an INTERNAL status code, as per
    // https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
    Status::new(tonic::Code::Internal, error.to_string())
}

/// A [`Codec`] that implements `application/grpc+json` via the prost library.
#[derive(Debug, Clone)]
struct JsonCodec<T, U> {
    _pd: PhantomData<(T, U)>,
}

impl<T, U> Default for JsonCodec<T, U> {
    fn default() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<T, U> Codec for JsonCodec<T, U>
    where
        T: serde::Serialize + Send + 'static,
        U: for<'a> serde::Deserialize<'a> + Send + Default + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = JsonEncoder<T>;
    type Decoder = JsonDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        JsonEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        JsonDecoder(PhantomData)
    }
}
