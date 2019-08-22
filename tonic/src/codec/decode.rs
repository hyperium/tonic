use crate::{Code, Status};
use bytes::{Buf, BufMut, BytesMut, IntoBuf};
use futures_core::{Stream, TryStream};
use futures_util::future;
use http::StatusCode;
use http_body::Body;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_codec::Decoder;
use tracing::{debug, trace};

pub fn decode_request<T, B>(
    decoder: T,
    source: B,
) -> impl TryStream<Ok = T::Item, Error = Status> + 'static
where
    T: Decoder<Error = Status> + 'static,
    T::Item: Unpin + 'static,
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    decode(decoder, source, Direction::Request)
}

pub fn decode_response<T, B>(
    decoder: T,
    source: B,
    status: StatusCode,
) -> impl TryStream<Ok = T::Item, Error = Status> + 'static
where
    T: Decoder<Error = Status> + 'static,
    T::Item: Unpin + 'static,
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    decode(decoder, source, Direction::Response(status))
}

pub fn decode_empty<T, B>(
    decoder: T,
    source: B,
) -> impl TryStream<Ok = T::Item, Error = Status> + 'static
where
    T: Decoder<Error = Status> + 'static,
    T::Item: Unpin + 'static,
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    decode(decoder, source, Direction::EmptyResponse)
}

pub struct Streaming<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>,
}

impl<T> Streaming<T> {
    pub fn new(inner: impl Stream<Item = Result<T, Status>> + Send + 'static) -> Self {
        let inner = Box::pin(inner);
        Self { inner }
    }
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<T> fmt::Debug for Streaming<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Streaming")
    }
}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody { compression: bool, len: usize },
}

#[derive(Debug)]
enum Direction {
    Request,
    Response(StatusCode),
    EmptyResponse,
}

fn decode<T, B>(
    mut decoder: T,
    mut source: B,
    direction: Direction,
) -> impl TryStream<Ok = T::Item, Error = Status> + 'static
where
    T: Decoder<Error = Status> + 'static,
    T::Item: Unpin + 'static,
    B: Body + 'static,
    B::Error: Into<crate::Error>,
{
    async_stream::try_stream! {
        let mut buf = BytesMut::with_capacity(1024 * 1024 * 1024);
        let mut state = State::ReadHeader;

        loop {
             if let Some(item) = decode_chunk(&mut decoder, &mut buf, &mut state)? {
                // TODO: implement the ability to poll trailers when we _know_ that
                // the comnsumer of this stream will only poll for the first message.
                // This means we skip the poll_trailers step.

                yield item;
            }

            // FIXME: Figure out how to verify that this is safe
            let chunk = match future::poll_fn(|cx| unsafe { std::pin::Pin::new_unchecked(&mut source) }.poll_data(cx)).await {
                Some(Ok(d)) => {
                    Some(d)
                },
                Some(Err(e)) => {
                    let err = e.into();
                    debug!("decoder inner stream error: {:?}", err);
                    let status = Status::from_error(&*err);
                    Err(status)?;
                    break;
                },
                None => None,
            };

            if let Some(data) = chunk {
                buf.put(data);
            } else {
                // FIXME: get BytesMut to impl `Buf` directlty?
                let buf1 = (&buf[..]).into_buf();
                if buf1.has_remaining() {
                    trace!("unexpected EOF decoding stream");
                    Err(Status::new(
                        Code::Internal,
                        "Unexpected EOF decoding stream.".to_string(),
                    ))?;
                } else {
                    break;
                }
            }
        }

        if let Direction::Response(status) = direction {
            let trailer = future::poll_fn(|cx| unsafe { std::pin::Pin::new_unchecked(&mut source) }.poll_trailers(cx));
            let trailer = match trailer.await {
                Ok(trailer) => crate::status::infer_grpc_status(trailer, status)?,
                Err(e) => {
                    let err = e.into();
                    debug!("decoder inner trailers error: {:?}", err);
                    let status = Status::from_error(&*err);
                    Err(status)?;
                },
                Ok(None) => return,
            };
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
