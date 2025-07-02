use bytes::{Buf, BufMut};
use protobuf::Message;
use std::marker::PhantomData;
use tonic::{
    codec::{BufferSettings, Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
    Status,
};

/// A [`Codec`] that implements `application/grpc+proto` via the protobuf
/// library.
#[derive(Debug, Clone)]
pub struct ProtoCodec<T, U> {
    _pd: PhantomData<(T, U)>,
}

impl<T, U> ProtoCodec<T, U> {
    /// Configure a ProstCodec with encoder/decoder buffer settings. This is used to control
    /// how memory is allocated and grows per RPC.
    pub fn new() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<T, U> Default for ProtoCodec<T, U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, U> Codec for ProtoCodec<T, U>
where
    T: Message + Send + 'static,
    U: Message + Default + Send + 'static,
{
    type Encode = T;
    type Decode = U;

    type Encoder = ProtoEncoder<T>;
    type Decoder = ProtoDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ProtoEncoder { _pd: PhantomData }
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProtoDecoder { _pd: PhantomData }
    }
}

/// A [`Encoder`] that knows how to encode `T`.
#[derive(Debug, Clone, Default)]
pub struct ProtoEncoder<T> {
    _pd: PhantomData<T>,
}

impl<T> ProtoEncoder<T> {
    /// Get a new encoder with explicit buffer settings
    pub fn new() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<T: Message> Encoder for ProtoEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        let serialized = item.serialize().map_err(from_decode_error)?;
        buf.put_slice(&serialized.as_slice());
        Ok(())
    }
}

/// A [`Decoder`] that knows how to decode `U`.
#[derive(Debug, Clone, Default)]
pub struct ProtoDecoder<U> {
    _pd: PhantomData<U>,
}

impl<U> ProtoDecoder<U> {
    /// Get a new decoder.
    pub fn new() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<U: Message + Default> Decoder for ProtoDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let slice = buf.chunk();
        let item = U::parse(&slice).map_err(from_decode_error)?;
        buf.advance(slice.len());
        Ok(Some(item))
    }
}

fn from_decode_error(error: impl std::error::Error) -> tonic::Status {
    // Map Protobuf parse errors to an INTERNAL status code, as per
    // https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
    Status::internal(error.to_string())
}
