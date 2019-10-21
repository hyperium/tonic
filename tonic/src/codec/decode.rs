use super::Decoder;
use crate::{
    body::BoxBody,
    codec::{BUFFER_SIZE, HEADER_SIZE},
    metadata::MetadataMap,
    Code, Status,
};
use bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};
use futures_core::Stream;
use futures_util::{future, ready};
use http::StatusCode;
use http_body::Body;
use std::{
    fmt,
    pin::Pin,
    task::{Context, Poll},
};
use tracing::{debug, trace};

/// Streaming requests and responses.
///
/// This will wrap some inner [`Body`] and [`Decoder`] and provide an interface
/// to fetch the message stream and trailing metadata
pub struct Streaming<T> {
    decoder: Box<dyn Decoder<Item = T, Error = Status> + Send + 'static>,
    body: BoxBody,
    state: State,
    direction: Direction,
    buf: BytesMut,
    trailers: Option<MetadataMap>,
}

impl<T> Unpin for Streaming<T> {}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody {
        is_compressed: bool,
        message_length: usize,
    },
}

#[derive(Debug)]
enum Direction {
    Request,
    Response(StatusCode),
    EmptyResponse,
}

impl<T> Streaming<T> {
    pub(crate) fn new_response<B, D>(decoder: D, body: B, status_code: StatusCode) -> Self
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes>,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(decoder, body, Direction::Response(status_code))
    }

    pub(crate) fn new_empty<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes>,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(decoder, body, Direction::EmptyResponse)
    }

    pub(crate) fn new_request<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes>,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(decoder, body, Direction::Request)
    }

    fn new<B, D>(decoder: D, body: B, direction: Direction) -> Self
    where
        B: Body + Send + 'static,
        B::Data: Into<Bytes>,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self {
            decoder: Box::new(decoder),
            body: BoxBody::map_from(body),
            state: State::ReadHeader,
            direction,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            trailers: None,
        }
    }
}

impl<T> Streaming<T> {
    /// Fetch the next message from this stream.
    /// ```rust
    /// # use tonic::{Streaming, Status};
    /// # use std::fmt::Debug;
    /// # async fn next_message_ex<T>(mut request: Streaming<T>) -> Result<(), Status>
    /// # where T: Debug
    /// # {
    /// if let Some(next_message) = request.message().await? {
    ///     println!("{:?}", next_message);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn message(&mut self) -> Result<Option<T>, Status> {
        match future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    /// Fetch the trailing metadata.
    ///
    /// This will drain the stream of all its messages to receive the trailing
    /// metadata. If [`Streaming::message`] returns `None` then this function
    /// will not need to poll for trailers since the body was totally consumed.
    ///
    /// ```rust
    /// # use tonic::{Streaming, Status};
    /// # async fn trailers_ex<T>(mut request: Streaming<T>) -> Result<(), Status> {
    /// if let Some(metadata) = request.trailers().await? {
    ///     println!("{:?}", metadata);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn trailers(&mut self) -> Result<Option<MetadataMap>, Status> {
        // Shortcut to see if we already pulled the trailers in the stream step
        // we need to do that so that the stream can error on trailing grpc-status
        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        // To fetch the trailers we must clear the body and drop it.
        while let Some(_) = self.message().await? {}

        // Since we call poll_trailers internally on poll_next we need to
        // check if it got cached again.
        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        // Trailers were not caught during poll_next and thus lets poll for
        // them manually.
        let map = future::poll_fn(|cx| Pin::new(&mut self.body).poll_trailers(cx))
            .await
            .map_err(|e| Status::from_error(&e))?;

        Ok(map.map(MetadataMap::from_headers))
    }

    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        // pull out the bytes containing the message
        let mut buf = (&self.buf[..]).into_buf();

        // read the tonic header
        if let State::ReadHeader = self.state {
            // if we don't have enough data from the body to decode the tonic header then read more
            // data
            if buf.remaining() < HEADER_SIZE {
                return Ok(None);
            }

            // FIXME: compression isn't supported yet, so just consume the first byte
            let is_compressed = match buf.get_u8() {
                0 => false,
                1 => {
                    trace!("message compressed, compression not supported yet");
                    return Err(Status::new(
                        Code::Unimplemented,
                        "Message compressed, compression not supported yet.".to_string(),
                    ));
                }
                f => {
                    trace!("unexpected compression flag");
                    return Err(Status::new(
                        Code::Internal,
                        format!("Unexpected compression flag: {}", f),
                    ));
                }
            };

            // consume the length of the message from the tonic header
            let message_length = buf.get_u32_be() as usize;

            // time to read the message body
            self.state = State::ReadBody {
                is_compressed,
                message_length,
            }
        }

        // read the message body
        if let State::ReadBody { message_length, .. } = self.state {
            // if we haven't read the entire message in then we need to wait for more data
            let bytes_left_to_decode = buf.remaining();
            if bytes_left_to_decode < message_length {
                return Ok(None);
            }

            // It's possible to read enough data to appear that we could decode the body, when in
            // fact the tonic + a _partial_ message body has been decoded.
            //
            // Example:
            //
            // Msg {
            //     bytes: Vec<u8>
            // }
            //
            // # Encode:
            // 1. Encode 10000 bytes from an instance of `Msg`
            // 2. Total bytes needed for decoding a message is:
            //    5 bytes for tonic's header
            //    + 10000 data bytes
            //    + 2 bytes to encode the number 10000
            //    + 1 tag byte
            //    ------------------------------------
            //    10008
            //
            // # Decode:
            // 1. Partial read of 10005 bytes from HTTP2 stream
            // 2. We've read enough bytes for the message, but we don't have the entire message
            //    because the first 5 bytes are tonic's header
            //
            //
            // If that's the case we need to wait for more data.
            let bytes_allocd_for_decoding = self.buf.len();
            let min_bytes_needed_to_decode = message_length + HEADER_SIZE;
            if bytes_allocd_for_decoding < min_bytes_needed_to_decode {
                return Ok(None);
            }

            // self.buf must always contain at least the length of the message number of bytes +
            // the number of bytes in the tonic header
            assert!(bytes_allocd_for_decoding >= min_bytes_needed_to_decode);

            // advance past the header
            self.buf.advance(HEADER_SIZE);

            match self.decoder.decode(&mut self.buf) {
                Ok(Some(msg)) => {
                    self.state = State::ReadHeader;
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
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // FIXME: implement the ability to poll trailers when we _know_ that
            // the consumer of this stream will only poll for the first message.
            // This means we skip the poll_trailers step.

            // if we're able to decode a chunk then the future is complete
            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            // otherwise wait for more data from the request body
            let body_chunk = match ready!(Pin::new(&mut self.body).poll_data(cx)) {
                Some(Ok(data)) => Some(data),
                Some(Err(e)) => {
                    let err: crate::Error = e.into();
                    debug!("decoder inner stream error: {:?}", err);
                    let status = Status::from_error(&*err);
                    Err(status)?;
                    break;
                }
                None => None,
            };

            // if we received some data from the body, ensure that self.buf has room and put data
            // into it
            if let Some(body_data) = body_chunk {
                let bytes_left_to_decode = body_data.remaining();
                let bytes_left_for_decoding = self.buf.remaining_mut();
                if bytes_left_to_decode > bytes_left_for_decoding {
                    let amt = bytes_left_to_decode.max(BUFFER_SIZE);
                    self.buf.reserve(amt);
                }

                self.buf.put(body_data);
            } else {
                // otherwise, ensure that there are no remaining bytes in self.buf
                //
                // FIXME: improve buf usage.
                let buf1 = (&self.buf[..]).into_buf();
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

        if let Direction::Response(status) = self.direction {
            match ready!(Pin::new(&mut self.body).poll_trailers(cx)) {
                Ok(trailer) => {
                    if let Err(e) = crate::status::infer_grpc_status(trailer.as_ref(), status) {
                        return Some(Err(e)).into();
                    } else {
                        self.trailers = trailer.map(MetadataMap::from_headers);
                    }
                }
                Err(e) => {
                    let err: crate::Error = e.into();
                    debug!("decoder inner trailers error: {:?}", err);
                    let status = Status::from_error(&*err);
                    return Some(Err(status)).into();
                }
            }
        }

        Poll::Ready(None)
    }
}

impl<T> fmt::Debug for Streaming<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Streaming").finish()
    }
}
