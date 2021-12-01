use std::marker::PhantomData;
use prost1::bytes::{Buf, BufMut};
use crate::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use crate::Status;

use serde;
use serde_json;

#[derive(Debug)]
pub struct JsonEncoder<T>(PhantomData<T>);

impl<T: serde::Serialize> Encoder for JsonEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let s = serde_json::to_string(&item).expect("Message only errors if not enough space");
        buf.put(s.as_bytes());

        Ok(())
    }
}

#[derive(Debug)]
pub struct JsonDecoder<U>(PhantomData<U>);

impl<U: serde::de::DeserializeOwned> Decoder for JsonDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let item = match serde_json::from_reader(buf.reader()) {
            Ok(i) => i,
            Err(e) => {
                return Err(from_decode_error(e));
            }
        };
        Ok(item)
    }
}

fn from_decode_error(error: serde_json::Error) -> Status {
    // Map Protobuf parse errors to an INTERNAL status code, as per
    // https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
    Status::new(crate::Code::Internal, error.to_string())
}

/// A [`Codec`] that implements `application/grpc+json` via the serde library.
#[derive(Debug, Clone)]
pub struct JsonCodec<T, U> {
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
        U: serde::de::DeserializeOwned + Send + 'static,
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

#[cfg(test)]
mod tests {
    use crate::codec::{Codec, Decoder, Encoder, JsonCodec};

    #[derive(serde_derive::Deserialize, serde_derive::Serialize, Debug)]
    struct Person {
        name: String,
    }

    #[test]
    fn json_codec() {
        let mut c = JsonCodec::default();

        let p = Person { name: "hello json-codec".into() };
        let mut bytes = bytes::BytesMut::with_capacity(100);
        let mut encode_buf = crate::codec::EncodeBuf::new(&mut bytes);
        let mut encoder = c.encoder();
        encoder.encode(p, &mut encode_buf).unwrap();
        assert_eq!(&bytes[..], b"{\"name\":\"hello json-codec\"}");

        let mut bytes = bytes::BytesMut::from(bytes);
        let len = bytes.len();
        let mut encode_buf = crate::codec::DecodeBuf::new(&mut bytes, len);
        let mut decoder = c.decoder();
        let p: Person = decoder.decode(&mut encode_buf).unwrap().unwrap();
        assert_eq!(p.name, "hello json-codec")
    }
}