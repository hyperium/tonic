//! This module defines common request/response types as well as the JsonCodec that is used by the
//! json.helloworld.Greeter service which is defined manually (instead of via proto files) by the
//! `build_json_codec_service` function in the `examples/build.rs` file.

use bytes::{Buf, BufMut};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use tonic::{
    codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    Status,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct HelloRequest {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HelloResponse {
    pub message: String,
}

#[derive(Debug)]
pub struct JsonEncoder<T>(PhantomData<T>);

impl<T: serde::Serialize> Encoder for JsonEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        serde_json::to_writer(buf.writer(), &item).map_err(|e| Status::internal(e.to_string()))
    }

    fn buffer_settings(&self) -> tonic::codec::BufferSettings {
        Default::default()
    }
}

#[derive(Debug)]
pub struct JsonDecoder<U>(PhantomData<U>);

impl<U: serde::de::DeserializeOwned> Decoder for JsonDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        if !buf.has_remaining() {
            return Ok(None);
        }

        let item: Self::Item =
            serde_json::from_reader(buf.reader()).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Some(item))
    }

    fn buffer_settings(&self) -> tonic::codec::BufferSettings {
        Default::default()
    }
}

/// A [`Codec`] that implements `application/grpc+json` via the serde library.
#[derive(Debug, Clone)]
pub struct JsonCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for JsonCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
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
