use super::compression::{decompress, CompressionEncoding};
use super::{DecodeBuf, Decoder, DEFAULT_MAX_RECV_MESSAGE_SIZE, HEADER_SIZE};
use crate::{body::BoxBody, metadata::MetadataMap, Code, Status};
use bytes::{Buf, BufMut, BytesMut};
use http::StatusCode;
use http_body::Body;
use std::{
    fmt, future,
    pin::Pin,
    task::ready,
    task::{Context, Poll},
};
use tokio_stream::Stream;
use tracing::{debug, trace};

const BUFFER_SIZE: usize = 8 * 1024;

/// Streaming requests and responses.
///
/// This will wrap some inner [`Body`] and [`Decoder`] and provide an interface
/// to fetch the message stream and trailing metadata
pub struct Streaming<T> {
    decoder: Box<dyn Decoder<Item = T, Error = Status> + Send + 'static>,
    inner: StreamingInner,
}

struct StreamingInner {
    body: BoxBody,
    state: State,
    direction: Direction,
    buf: BytesMut,
    trailers: Option<MetadataMap>,
    decompress_buf: BytesMut,
    encoding: Option<CompressionEncoding>,
    max_message_size: Option<usize>,
}

impl<T> Unpin for Streaming<T> {}

#[derive(Debug, Clone, Copy)]
enum State {
    ReadHeader,
    ReadBody {
        compression: Option<CompressionEncoding>,
        len: usize,
    },
    Error,
}

#[derive(Debug, PartialEq, Eq)]
enum Direction {
    Request,
    Response(StatusCode),
    EmptyResponse,
}

impl<T> Streaming<T> {
    pub(crate) fn new_response<B, D>(
        decoder: D,
        body: B,
        status_code: StatusCode,
        encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(
            decoder,
            body,
            Direction::Response(status_code),
            encoding,
            max_message_size,
        )
    }

    pub(crate) fn new_empty<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(decoder, body, Direction::EmptyResponse, None, None)
    }

    #[doc(hidden)]
    pub fn new_request<B, D>(
        decoder: D,
        body: B,
        encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(
            decoder,
            body,
            Direction::Request,
            encoding,
            max_message_size,
        )
    }

    fn new<B, D>(
        decoder: D,
        body: B,
        direction: Direction,
        encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self {
            decoder: Box::new(decoder),
            inner: StreamingInner {
                body: body
                    .map_data(|mut buf| buf.copy_to_bytes(buf.remaining()))
                    .map_err(|err| Status::map_error(err.into()))
                    .boxed_unsync(),
                state: State::ReadHeader,
                direction,
                buf: BytesMut::with_capacity(BUFFER_SIZE),
                trailers: None,
                decompress_buf: BytesMut::new(),
                encoding,
                max_message_size,
            },
        }
    }
}

impl StreamingInner {
    fn decode_chunk(&mut self) -> Result<Option<DecodeBuf<'_>>, Status> {
        if let State::ReadHeader = self.state {
            if self.buf.remaining() < HEADER_SIZE {
                return Ok(None);
            }

            let compression_encoding = match self.buf.get_u8() {
                0 => None,
                1 => {
                    {
                        if self.encoding.is_some() {
                            self.encoding
                        } else {
                            // https://grpc.github.io/grpc/core/md_doc_compression.html
                            // An ill-constructed message with its Compressed-Flag bit set but lacking a grpc-encoding
                            // entry different from identity in its metadata MUST fail with INTERNAL status,
                            // its associated description indicating the invalid Compressed-Flag condition.
                            return Err(Status::new(Code::Internal, "protocol error: received message with compressed-flag but no grpc-encoding was specified"));
                        }
                    }
                }
                f => {
                    trace!("unexpected compression flag");
                    let message = if let Direction::Response(status) = self.direction {
                        format!(
                            "protocol error: received message with invalid compression flag: {} (valid flags are 0 and 1) while receiving response with status: {}",
                            f, status
                        )
                    } else {
                        format!("protocol error: received message with invalid compression flag: {} (valid flags are 0 and 1), while sending request", f)
                    };
                    return Err(Status::new(Code::Internal, message));
                }
            };

            let len = self.buf.get_u32() as usize;
            let limit = self
                .max_message_size
                .unwrap_or(DEFAULT_MAX_RECV_MESSAGE_SIZE);
            if len > limit {
                return Err(Status::new(
                    Code::OutOfRange,
                    format!(
                        "Error, message length too large: found {} bytes, the limit is: {} bytes",
                        len, limit
                    ),
                ));
            }

            self.buf.reserve(len);

            self.state = State::ReadBody {
                compression: compression_encoding,
                len,
            }
        }

        if let State::ReadBody { len, compression } = self.state {
            // if we haven't read enough of the message then return and keep
            // reading
            if self.buf.remaining() < len || self.buf.len() < len {
                return Ok(None);
            }

            let decode_buf = if let Some(encoding) = compression {
                self.decompress_buf.clear();

                if let Err(err) = decompress(encoding, &mut self.buf, &mut self.decompress_buf, len)
                {
                    let message = if let Direction::Response(status) = self.direction {
                        format!(
                            "Error decompressing: {}, while receiving response with status: {}",
                            err, status
                        )
                    } else {
                        format!("Error decompressing: {}, while sending request", err)
                    };
                    return Err(Status::new(Code::Internal, message));
                }
                let decompressed_len = self.decompress_buf.len();
                DecodeBuf::new(&mut self.decompress_buf, decompressed_len)
            } else {
                DecodeBuf::new(&mut self.buf, len)
            };

            return Ok(Some(decode_buf));
        }

        Ok(None)
    }

    // Returns Some(()) if data was found or None if the loop in `poll_next` should break
    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<()>, Status>> {
        let chunk = match ready!(Pin::new(&mut self.body).poll_data(cx)) {
            Some(Ok(d)) => Some(d),
            Some(Err(status)) => {
                if self.direction == Direction::Request && status.code() == Code::Cancelled {
                    return Poll::Ready(Ok(None));
                }

                let _ = std::mem::replace(&mut self.state, State::Error);
                debug!("decoder inner stream error: {:?}", status);
                return Poll::Ready(Err(status));
            }
            None => None,
        };

        Poll::Ready(if let Some(data) = chunk {
            self.buf.put(data);
            Ok(Some(()))
        } else {
            // FIXME: improve buf usage.
            if self.buf.has_remaining() {
                trace!("unexpected EOF decoding stream, state: {:?}", self.state);
                Err(Status::new(
                    Code::Internal,
                    "Unexpected EOF decoding stream.".to_string(),
                ))
            } else {
                Ok(None)
            }
        })
    }

    fn poll_response(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Status>> {
        if let Direction::Response(status) = self.direction {
            match ready!(Pin::new(&mut self.body).poll_trailers(cx)) {
                Ok(trailer) => {
                    if let Err(e) = crate::status::infer_grpc_status(trailer.as_ref(), status) {
                        if let Some(e) = e {
                            return Poll::Ready(Err(e));
                        } else {
                            return Poll::Ready(Ok(()));
                        }
                    } else {
                        self.trailers = trailer.map(MetadataMap::from_headers);
                    }
                }
                Err(status) => {
                    debug!("decoder inner trailers error: {:?}", status);
                    return Poll::Ready(Err(status));
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<T> Streaming<T> {
    /// Fetch the next message from this stream.
    ///
    /// # Return value
    ///
    /// - `Result::Err(val)` means a gRPC error was sent by the sender instead
    /// of a valid response message. Refer to [`Status::code`] and
    /// [`Status::message`] to examine possible error causes.
    ///
    /// - `Result::Ok(None)` means the stream was closed by the sender and no
    /// more messages will be delivered. Further attempts to call
    /// [`Streaming::message`] will result in the same return value.
    ///
    /// - `Result::Ok(Some(val))` means the sender streamed a valid response
    /// message `val`.
    ///
    /// ```rust
    /// # use tonic::{Streaming, Status, codec::Decoder};
    /// # use std::fmt::Debug;
    /// # async fn next_message_ex<T, D>(mut request: Streaming<T>) -> Result<(), Status>
    /// # where T: Debug,
    /// # D: Decoder<Item = T, Error = Status> + Send  + 'static,
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
        if let Some(trailers) = self.inner.trailers.take() {
            return Ok(Some(trailers));
        }

        // To fetch the trailers we must clear the body and drop it.
        while self.message().await?.is_some() {}

        // Since we call poll_trailers internally on poll_next we need to
        // check if it got cached again.
        if let Some(trailers) = self.inner.trailers.take() {
            return Ok(Some(trailers));
        }

        // Trailers were not caught during poll_next and thus lets poll for
        // them manually.
        let map = future::poll_fn(|cx| Pin::new(&mut self.inner.body).poll_trailers(cx))
            .await
            .map_err(|e| Status::from_error(Box::new(e)));

        map.map(|x| x.map(MetadataMap::from_headers))
    }

    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        match self.inner.decode_chunk()? {
            Some(mut decode_buf) => match self.decoder.decode(&mut decode_buf)? {
                Some(msg) => {
                    self.inner.state = State::ReadHeader;
                    Ok(Some(msg))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let State::Error = &self.inner.state {
                return Poll::Ready(None);
            }

            // FIXME: implement the ability to poll trailers when we _know_ that
            // the consumer of this stream will only poll for the first message.
            // This means we skip the poll_trailers step.
            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            match ready!(self.inner.poll_data(cx))? {
                Some(()) => (),
                None => break,
            }
        }

        Poll::Ready(match ready!(self.inner.poll_response(cx)) {
            Ok(()) => None,
            Err(err) => Some(Err(err)),
        })
    }
}

impl<T> fmt::Debug for Streaming<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Streaming").finish()
    }
}

#[cfg(test)]
static_assertions::assert_impl_all!(Streaming<()>: Send);
