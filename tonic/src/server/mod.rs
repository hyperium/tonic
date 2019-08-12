#![allow(dead_code)]

use crate::{Code, Request, Response, Status};
use async_stream::stream;
use bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};
use futures_core::{Stream, TryStream};
use futures_util::{future, stream, StreamExt, TryStreamExt};
use http_body::Body;
use std::future::Future;
use tokio_codec::{Decoder, Encoder};
use tower_service::Service;
use tracing::{debug, trace};

pub trait Codec {
    type Encode;
    type Decode;
}

pub struct Encode<T, U> {
    encoder: T,
    source: U,
}

impl<T, U> Encode<T, U>
where
    T: Encoder,
    U: TryStream<Ok = T::Item, Error = Status> + Unpin,
{
    pub fn new(encoder: T, source: U) -> Self {
        Encode { encoder, source }
    }

    pub fn encode<'a>(
        &'a mut self,
        buf: &'a mut BytesMut,
    ) -> impl Stream<Item = Result<crate::body::BytesBuf, Status>> + 'a {
        stream! {
            loop {
                match self.source.try_next().await {
                    Ok(Some(item)) => {
                        self.encoder.encode(item, buf).map_err(drop).unwrap();
                        let len = buf.len();
                        yield Ok(buf.split_to(len).freeze().into_buf());
                    },
                    Ok(None) => break,
                    Err(status) => yield Err(status),
                }
            }
        }
    }
}

pub struct Streaming<T> {
    decoder: T,
    buf: BytesMut,
    state: State,
}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody { compression: bool, len: usize },
    Done,
}

impl<T> Streaming<T>
where
    T: Decoder,
    T::Item: Unpin + 'static,
{
    pub fn decode<'a, B>(
        &'a mut self,
        source: &'a mut B,
    ) -> impl Stream<Item = Result<T::Item, Status>> + 'a
    where
        B: Body,
        B::Error: Into<crate::Error>,
    {
        stream! {
            loop {
                // TODO: use try_stream! and ?
                if let Some(item) = self.decode_chunk().unwrap() {
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
                    self.buf.put(data);
                } else {
                    if self.buf.has_remaining_mut() {
                        trace!("unexpected EOF decoding stream");
                        yield Err(Status::new(
                            Code::Internal,
                            "Unexpected EOF decoding stream.".to_string(),
                        ));
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn decode_chunk(&mut self) -> Result<Option<T::Item>, Status> {
        let buf = (&self.buf).into_buf();

        if let State::ReadHeader = self.state {
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
            let len = (&self.buf[..]).into_buf().get_u32_be() as usize;

            self.state = State::ReadBody {
                compression: is_compressed,
                len,
            }
        }

        if let State::ReadBody { len, .. } = self.state {
            if buf.remaining() < len {
                return Ok(None);
            }

            match self.decoder.decode(&mut self.buf) {
                Ok(Some(msg)) => {
                    self.state = State::ReadHeader;
                    return Ok(Some(msg));
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use crate::body::AsyncBody;
    use crate::server::Encode;
    use bytes::{Bytes, BytesMut};
    use tokio_codec::BytesCodec;

    #[test]
    fn body() {
        let stream = futures_util::stream::iter(vec![Ok(Bytes::new())]);
        let mut encode = Encode::new(BytesCodec::new(), stream);

        let mut buf = BytesMut::with_capacity(1024);
        AsyncBody::new(Box::pin(encode.encode(&mut buf)));
    }
}

// impl<T, U> http_body::Body for Encode<T, U>

// pub struct Grpc<T> {
//     opdec: T,
// }

// impl<T: Codec> Grpc<T> {
//     pub async fn unary<B>(&mut self, message: B) -> Result<Response<B>> {
//         self.server_streaming(stream::once(message)).await
//     }

//     pub async fn server_streaming<B>(
//         &mut self,
//         stream: impl Stream,
//     ) -> Result<Response<impl http_body::Body>> {
//         unimplemetned!()
//     }

//     fn map_request<B>(&mut self, request: http::Request<B>) -> Request<B> {
//         Request::from_http(request)
//     }
// }
