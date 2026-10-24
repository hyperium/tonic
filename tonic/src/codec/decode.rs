/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use super::compression::{CompressionEncoding, CompressionSettings, decompress};
use super::{BufferSettings, DEFAULT_MAX_RECV_MESSAGE_SIZE, DecodeBuf, Decoder, HEADER_SIZE};
use crate::{Code, Status, body::Body, metadata::MetadataMap};
use bytes::{Buf, BufMut, BytesMut};
use http::{HeaderMap, StatusCode};
use http_body::Body as HttpBody;
use http_body_util::BodyExt;
use std::{
    fmt, future,
    pin::Pin,
    task::ready,
    task::{Context, Poll},
};
use sync_wrapper::SyncWrapper;
use tokio_stream::Stream;
use tracing::{debug, trace};

/// Streaming requests and responses.
///
/// This will wrap some inner [`Body`] and [`Decoder`] and provide an interface
/// to fetch the message stream and trailing metadata
pub struct Streaming<T> {
    decoder: SyncWrapper<Box<dyn Decoder<Item = T, Error = Status> + Send + 'static>>,
    inner: StreamingInner,
}

struct StreamingInner {
    body: SyncWrapper<Body>,
    state: State,
    direction: Direction,
    buf: BytesMut,
    trailers: Option<HeaderMap>,
    decompress_buf: BytesMut,
    encoding: Option<CompressionEncoding>,
    max_message_size: Option<usize>,
}

impl<T> Unpin for Streaming<T> {}

#[derive(Debug, Clone)]
enum State {
    ReadHeader,
    ReadBody {
        compression: Option<CompressionEncoding>,
        len: usize,
    },
    Error(Option<Status>),
}

#[derive(Debug, PartialEq, Eq)]
enum Direction {
    Request,
    Response(StatusCode),
    EmptyResponse,
}

impl<T> Streaming<T> {
    /// Create a new streaming response in the grpc response format for decoding a response [Body]
    /// into message of type T
    pub fn new_response<B, D>(
        decoder: D,
        body: B,
        status_code: StatusCode,
        encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self
    where
        B: HttpBody + Send + 'static,
        B::Error: Into<crate::BoxError>,
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

    /// Create empty response. For creating responses that have no content (headers + trailers only)
    pub fn new_empty<B, D>(decoder: D, body: B) -> Self
    where
        B: HttpBody + Send + 'static,
        B::Error: Into<crate::BoxError>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(decoder, body, Direction::EmptyResponse, None, None)
    }

    /// Create a new streaming request in the grpc response format for decoding a request [Body]
    /// into message of type T
    pub fn new_request<B, D>(
        decoder: D,
        body: B,
        encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self
    where
        B: HttpBody + Send + 'static,
        B::Error: Into<crate::BoxError>,
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
        B: HttpBody + Send + 'static,
        B::Error: Into<crate::BoxError>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        let buffer_size = decoder.buffer_settings().buffer_size;
        Self {
            decoder: SyncWrapper::new(Box::new(decoder)),
            inner: StreamingInner {
                body: SyncWrapper::new(Body::new(
                    body.map_frame(|frame| {
                        frame.map_data(|mut buf| buf.copy_to_bytes(buf.remaining()))
                    })
                    .map_err(|err| Status::map_error(err.into())),
                )),
                state: State::ReadHeader,
                direction,
                buf: BytesMut::with_capacity(buffer_size),
                trailers: None,
                decompress_buf: BytesMut::new(),
                encoding,
                max_message_size,
            },
        }
    }
}

impl StreamingInner {
    fn decode_chunk(
        &mut self,
        buffer_settings: BufferSettings,
    ) -> Result<Option<DecodeBuf<'_>>, Status> {
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
                            return Err(Status::internal(
                                "protocol error: received message with compressed-flag but no grpc-encoding was specified",
                            ));
                        }
                    }
                }
                f => {
                    trace!("unexpected compression flag");
                    let message = if let Direction::Response(status) = self.direction {
                        format!(
                            "protocol error: received message with invalid compression flag: {f} (valid flags are 0 and 1) while receiving response with status: {status}"
                        )
                    } else {
                        format!(
                            "protocol error: received message with invalid compression flag: {f} (valid flags are 0 and 1), while sending request"
                        )
                    };
                    return Err(Status::internal(message));
                }
            };

            let len = self.buf.get_u32() as usize;
            let limit = self
                .max_message_size
                .unwrap_or(DEFAULT_MAX_RECV_MESSAGE_SIZE);
            if len > limit {
                return Err(Status::out_of_range(format!(
                    "Error, decoded message length too large: found {len} bytes, the limit is: {limit} bytes"
                )));
            }

            let missing = len.saturating_sub(self.buf.len());
            if self.buf.capacity() - self.buf.len() < missing {
                let mut buf = BytesMut::with_capacity(self.buf.len() + missing);
                buf.extend_from_slice(&self.buf);
                self.buf = buf;
            }

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
                let limit = self
                    .max_message_size
                    .unwrap_or(DEFAULT_MAX_RECV_MESSAGE_SIZE);
                let limited_out_buf = (&mut self.decompress_buf).limit(limit);

                if let Err(err) = decompress(
                    CompressionSettings {
                        encoding,
                        buffer_growth_interval: buffer_settings.buffer_size,
                    },
                    &mut self.buf,
                    limited_out_buf,
                    len,
                ) {
                    if matches!(err.kind(), std::io::ErrorKind::WriteZero) {
                        return Err(Status::resource_exhausted(format!(
                            "Error decompressing: size limit, of {limit} bytes, exceeded while decompressing message"
                        )));
                    }
                    let message = if let Direction::Response(status) = self.direction {
                        format!(
                            "Error decompressing: {err}, while receiving response with status: {status}"
                        )
                    } else {
                        format!("Error decompressing: {err}, while sending request")
                    };
                    return Err(Status::internal(message));
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
    fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<()>, Status>> {
        let frame = match ready!(Pin::new(self.body.get_mut()).poll_frame(cx)) {
            Some(Ok(frame)) => frame,
            Some(Err(status)) => {
                if self.direction == Direction::Request && status.code() == Code::Cancelled {
                    return Poll::Ready(Ok(None));
                }

                let _ = std::mem::replace(&mut self.state, State::Error(Some(status.clone())));
                debug!("decoder inner stream error: {:?}", status);
                return Poll::Ready(Err(status));
            }
            None => {
                // FIXME: improve buf usage.
                return Poll::Ready(if self.buf.has_remaining() {
                    trace!("unexpected EOF decoding stream, state: {:?}", self.state);
                    Err(Status::internal("Unexpected EOF decoding stream."))
                } else {
                    Ok(None)
                });
            }
        };

        Poll::Ready(if frame.is_data() {
            self.buf.put(frame.into_data().unwrap());
            Ok(Some(()))
        } else if frame.is_trailers() {
            if let Some(trailers) = &mut self.trailers {
                trailers.extend(frame.into_trailers().unwrap());
            } else {
                self.trailers = Some(frame.into_trailers().unwrap());
            }

            Ok(None)
        } else {
            panic!("unexpected frame: {frame:?}");
        })
    }

    fn response(&mut self) -> Result<(), Status> {
        if let Direction::Response(status) = self.direction
            && let Err(Some(e)) = crate::status::infer_grpc_status(self.trailers.as_ref(), status)
        {
            // If the trailers contain a grpc-status, then we should return that as the error
            // and otherwise stop the stream (by taking the error state)
            self.trailers.take();
            return Err(e);
        }
        Ok(())
    }
}

impl<T> Streaming<T> {
    /// Fetch the next message from this stream.
    ///
    /// # Return value
    ///
    /// - `Result::Err(val)` means a gRPC error was sent by the sender instead
    ///   of a valid response message. Refer to [`Status::code`] and
    ///   [`Status::message`] to examine possible error causes.
    ///
    /// - `Result::Ok(None)` means the stream was closed by the sender and no
    ///   more messages will be delivered. Further attempts to call
    ///   [`Streaming::message`] will result in the same return value.
    ///
    /// - `Result::Ok(Some(val))` means the sender streamed a valid response
    ///   message `val`.
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
            return Ok(Some(MetadataMap::from_headers(trailers)));
        }

        // To fetch the trailers we must clear the body and drop it.
        while self.message().await?.is_some() {}

        // Since we call poll_trailers internally on poll_next we need to
        // check if it got cached again.
        if let Some(trailers) = self.inner.trailers.take() {
            return Ok(Some(MetadataMap::from_headers(trailers)));
        }

        // We've polled through all the frames, and still no trailers, return None
        Ok(None)
    }

    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        match self
            .inner
            .decode_chunk(self.decoder.get_mut().buffer_settings())?
        {
            Some(mut decode_buf) => match self.decoder.get_mut().decode(&mut decode_buf)? {
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
            // When the stream encounters an error yield that error once and then on subsequent
            // calls to poll_next return Poll::Ready(None) indicating that the stream has been
            // fully exhausted.
            if let State::Error(status) = &mut self.inner.state {
                return Poll::Ready(status.take().map(Err));
            }

            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            if ready!(self.inner.poll_frame(cx))?.is_none() {
                match self.inner.response() {
                    Ok(()) => return Poll::Ready(None),
                    Err(err) => self.inner.state = State::Error(Some(err)),
                }
            }
        }
    }
}

impl<T> fmt::Debug for Streaming<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Streaming").finish()
    }
}

#[cfg(test)]
static_assertions::assert_impl_all!(Streaming<()>: Send, Sync);

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body::Frame;

    #[derive(Debug)]
    struct BytesDecoder;

    impl Decoder for BytesDecoder {
        type Item = Bytes;
        type Error = Status;

        fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
            Ok(Some(buf.copy_to_bytes(buf.remaining())))
        }
    }

    fn message(len: usize) -> Bytes {
        let mut buf = BytesMut::with_capacity(HEADER_SIZE + len);
        buf.put_u8(0);
        buf.put_u32(len as u32);
        buf.put_bytes(7, len);
        buf.freeze()
    }

    fn streaming(mut wire: Bytes, frame_sizes: &[usize]) -> Streaming<Bytes> {
        let frames: Vec<_> = frame_sizes
            .iter()
            .map(|&size| Ok::<_, Status>(Frame::data(wire.split_to(size))))
            .collect();
        assert!(wire.is_empty());
        let body = http_body_util::StreamBody::new(tokio_stream::iter(frames));
        Streaming::new_request(BytesDecoder, body, None, Some(8 * 1024 * 1024))
    }

    #[tokio::test]
    async fn decode_buffer_is_sized_to_the_message() {
        const LEN: usize = 2 * 1024 * 1024;
        const TOTAL: usize = HEADER_SIZE + LEN;
        let mut small_frames = vec![16 * 1024; TOTAL / (16 * 1024)];
        small_frames.push(TOTAL % (16 * 1024));
        let cases: [&[usize]; 4] = [
            &[TOTAL],
            &[4096, TOTAL - 4096],
            &[1536 * 1024, TOTAL - 1536 * 1024],
            &small_frames,
        ];
        for frame_sizes in cases {
            let mut stream = streaming(message(LEN), frame_sizes);
            assert_eq!(stream.message().await.unwrap().unwrap().len(), LEN);
            assert_eq!(stream.inner.buf.capacity(), 0, "frames: {frame_sizes:?}");
            assert!(stream.message().await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn decodes_several_messages_in_one_frame() {
        let wire: Bytes = [10, 20, 30].into_iter().flat_map(message).collect();
        let total = wire.len();
        let mut stream = streaming(wire, &[total]);
        for len in [10, 20, 30] {
            assert_eq!(stream.message().await.unwrap().unwrap().len(), len);
        }
        assert!(stream.message().await.unwrap().is_none());
    }
}
