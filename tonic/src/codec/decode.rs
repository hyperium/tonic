use crate::{Code, Status};
use bytes::{Buf, BufMut, BytesMut, IntoBuf};
use futures_core::{Stream, TryStream};
use futures_util::future;
use http::StatusCode;
use http_body::Body;
use std::pin::Pin;
use tokio_codec::Decoder;
use tracing::{debug, trace};

pub struct Streaming<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>,
}

impl<T> Streaming<T> {
    pub fn new(inner: impl Stream<Item = Result<T, Status>> + Send + 'static) -> Self {
        let inner = Box::pin(inner);
        Self { inner }
    }
}

use std::task::{Context, Poll};
impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody { compression: bool, len: usize },
}

enum Direction {
    Request,
    Response(StatusCode),
    EmptyResponse,
}

pub fn decode<T, B>(
    mut decoder: T,
    mut source: B,
) -> impl TryStream<Ok = T::Item, Error = Status> + 'static
where
    T: Decoder<Error = Status> + 'static,
    T::Item: Unpin + 'static,
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    async_stream::stream! {
        let mut buf = BytesMut::with_capacity(1024 * 1024);
        let mut state = State::ReadHeader;

        loop {
            // TODO: use try_stream! and ?
            if let Some(item) = decode_chunk(&mut decoder, &mut buf, &mut state).unwrap() {
                yield Ok(item);
            }

            // FIXME: Figure out how to verify that this is safe
            let chunk = match future::poll_fn(|cx| unsafe { std::pin::Pin::new_unchecked(&mut source) }.poll_data(cx)).await {
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

            if let Some(data) = chunk {
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
