#![allow(dead_code)]

use crate::{body::BytesBuf, Code, Status};
use async_stream::stream;
use bytes::{Buf, BufMut, BytesMut, IntoBuf};
use futures_core::TryStream;
use futures_util::{future, TryStreamExt};
use http_body::Body;
use prost::Message;
use std::marker::PhantomData;
use tokio_codec::{Decoder, Encoder};
use tracing::{debug, trace};

pub trait Codec {
    type Encode;
    type Decode;

    type Encoder: Encoder<Item = Self::Encode, Error = Status>;
    type Decoder: Decoder<Item = Self::Decode, Error = Status>;

    const CONTENT_TYPE: &'static str;

    fn encoder(&mut self) -> Self::Encoder;
    fn decoder(&mut self) -> Self::Decoder;
}

pub fn encode<T, U>(mut encoder: T, mut source: U) -> impl TryStream<Ok = BytesBuf, Error = Status>
where
    T: Encoder<Error = Status>,
    U: TryStream<Ok = T::Item, Error = Status> + Unpin,
{
    stream! {
        let mut buf = BytesMut::with_capacity(1024);

        loop {
            match source.try_next().await {
                Ok(Some(item)) => {


                    buf.reserve(5);
                    unsafe {
                        buf.advance_mut(5);
                    }
                    encoder.encode(item, &mut buf).map_err(drop).unwrap();

                    // now that we know length, we can write the header
                    let len = buf.len() - 5;
                    assert!(len <= ::std::u32::MAX as usize);
                    {
                        let mut cursor = ::std::io::Cursor::new(&mut buf[..5]);
                        cursor.put_u8(0); // byte must be 0, reserve doesn't auto-zero
                        cursor.put_u32_be(len as u32);
                    }

                    yield Ok(buf.split_to(len + 5).freeze().into_buf());
                },
                Ok(None) => break,
                Err(status) => yield Err(status),
            }
        }
    }
}

pub fn decode<T, B>(mut decoder: T, mut source: B) -> impl TryStream<Ok = T::Item, Error = Status>
where
    T: Decoder<Error = Status>,
    T::Item: Unpin + 'static,
    B: Body,
    B::Error: Into<crate::Error>,
{
    stream! {
        let mut buf = BytesMut::with_capacity(1024);
        let mut state = State::ReadHeader;

        loop {
            // TODO: use try_stream! and ?
            if let Some(item) = decode_chunk(&mut decoder, &mut buf, &mut state).unwrap() {
                yield Ok(item);
            }

            let chunk = match future::poll_fn(|cx| source.poll_data(cx)).await {
                Some(Ok(d)) => Some(d),
                Some(Err(e)) => {
                    let err = e.into();
                    debug!("decoder inner stream error: {:?}", err);
                    let status = Status::from_error(&*err);
                    yield Err(status);
                    break;
                },
                None => None,
            };

            if let Some(data)= chunk {
                buf.put(data);
            } else {
                if buf.has_remaining_mut() {
                    trace!("unexpected EOF decoding stream");
                    yield Err(Status::new(
                        Code::Internal,
                        "Unexpected EOF decoding stream.".to_string(),
                    ));
                } else {
                    break;
                }
            }

            // TODO: poll_trailers for Response status code
        }
    }
}

fn decode_chunk<T>(
    decoder: &mut T,
    buf1: &mut BytesMut,
    state: &mut State,
) -> Result<Option<T::Item>, Status>
where
    T: Decoder<Error = Status>,
{
    let mut buf = (&buf1[..]).into_buf();

    if let State::ReadHeader = state {
        println!("reading header");
        if buf.remaining() < 5 {
            return Ok(None);
        }

        let is_compressed = match buf.get_u8() {
            0 => false,
            1 => {
                trace!("message compressed, compression not supported yet");
                return Err(crate::Status::new(
                    crate::Code::Unimplemented,
                    "Message compressed, compression not supported yet.".to_string(),
                ));
            }
            f => {
                trace!("unexpected compression flag");
                return Err(crate::Status::new(
                    crate::Code::Internal,
                    format!("Unexpected compression flag: {}", f),
                ));
            }
        };
        let len = buf.get_u32_be() as usize;

        *state = State::ReadBody {
            compression: is_compressed,
            len,
        }
    }

    if let State::ReadBody { len, .. } = state {
        println!("reading body");
        if buf.remaining() < *len {
            return Ok(None);
        }

        // advance past the header
        buf1.advance(5);

        match decoder.decode(buf1) {
            Ok(Some(msg)) => {
                *state = State::ReadHeader;
                return Ok(Some(msg));
            }
            Ok(None) => return Ok(None),
            Err(e) => {
                return Err(e);
            }
        }
    }

    Ok(None)
}

#[derive(Debug, Clone)]
pub struct ProstCodec<T, U> {
    _pd: PhantomData<(T, U)>,
}

impl<T, U> ProstCodec<T, U> {
    pub fn new() -> Self {
        Self { _pd: PhantomData }
    }
}

impl<T, U> Codec for ProstCodec<T, U>
where
    T: Message,
    U: Message + Default,
{
    type Encode = T;
    type Decode = U;

    type Encoder = ProstEncoder<T>;
    type Decoder = ProstDecoder<U>;

    const CONTENT_TYPE: &'static str = "application/groc+proto";

    fn encoder(&mut self) -> Self::Encoder {
        ProstEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstDecoder(PhantomData)
    }
}

pub struct ProstEncoder<T>(PhantomData<T>);

impl<T: Message> Encoder for ProstEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
        let len = item.encoded_len();

        if buf.remaining_mut() < len {
            buf.reserve(len);
        }

        item.encode(buf)
            .map_err(|_| unreachable!("Message only errors if not enough space"))
    }
}

pub struct ProstDecoder<U>(PhantomData<U>);

impl<U: Message + Default> Decoder for ProstDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Message::decode(buf.take())
            .map(Option::Some)
            .map_err(from_decode_error)
    }
}

fn from_decode_error(error: prost::DecodeError) -> crate::Status {
    // Map Protobuf parse errors to an INTERNAL status code, as per
    // https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
    crate::Status::new(crate::Code::Internal, error.to_string())
}

#[derive(Default)]
pub struct UnitCodec;

impl Codec for UnitCodec {
    type Encode = ();
    type Decode = ();

    type Encoder = UnitEncoder;
    type Decoder = UnitDecoder;

    const CONTENT_TYPE: &'static str = "()";

    fn encoder(&mut self) -> Self::Encoder {
        UnitEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        UnitDecoder
    }
}

pub struct UnitEncoder;

impl Encoder for UnitEncoder {
    type Item = ();
    type Error = crate::Status;

    fn encode(&mut self, _item: Self::Item, _buf: &mut BytesMut) -> Result<(), Self::Error> {
        unimplemented!()
    }
}

pub struct UnitDecoder;

impl Decoder for UnitDecoder {
    type Item = ();
    type Error = Status;

    fn decode(&mut self, _buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Ok(Some(()))
    }
}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody { compression: bool, len: usize },
    Done,
}
