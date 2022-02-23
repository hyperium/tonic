#[cfg(feature = "compression")]
use super::compression::{decompress, CompressionEncoding};
use super::{DecodeBuf, Decoder, HEADER_SIZE};
use crate::{body::BoxBody, metadata::MetadataMap, Code, Status};
use bytes::{Buf, BufMut, BytesMut};
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

const BUFFER_SIZE: usize = 8 * 1024;

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
    #[cfg(feature = "compression")]
    decompress_buf: BytesMut,
    #[cfg(feature = "compression")]
    encoding: Option<CompressionEncoding>,
}

impl<T> Unpin for Streaming<T> {}

#[derive(Debug)]
enum State {
    ReadHeader,
    ReadBody { compression: bool, len: usize },
    Error,
}

#[derive(Debug)]
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
        #[cfg(feature = "compression")] encoding: Option<CompressionEncoding>,
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
            #[cfg(feature = "compression")]
            encoding,
        )
    }

    pub(crate) fn new_empty<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self::new(
            decoder,
            body,
            Direction::EmptyResponse,
            #[cfg(feature = "compression")]
            None,
        )
    }

    #[doc(hidden)]
    pub fn new_request<B, D>(
        decoder: D,
        body: B,
        #[cfg(feature = "compression")] encoding: Option<CompressionEncoding>,
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
            #[cfg(feature = "compression")]
            encoding,
        )
    }

    fn new<B, D>(
        decoder: D,
        body: B,
        direction: Direction,
        #[cfg(feature = "compression")] encoding: Option<CompressionEncoding>,
    ) -> Self
    where
        B: Body + Send + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + 'static,
    {
        Self {
            decoder: Box::new(decoder),
            body: body
                .map_data(|mut buf| buf.copy_to_bytes(buf.remaining()))
                .map_err(|err| Status::map_error(err.into()))
                .boxed_unsync(),
            state: State::ReadHeader,
            direction,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            trailers: None,
            #[cfg(feature = "compression")]
            decompress_buf: BytesMut::new(),
            #[cfg(feature = "compression")]
            encoding,
        }
    }
}

impl<T> Streaming<T> {
    /// Fetch the next message from this stream.
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
        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        // To fetch the trailers we must clear the body and drop it.
        while self.message().await?.is_some() {}

        // Since we call poll_trailers internally on poll_next we need to
        // check if it got cached again.
        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        // Trailers were not caught during poll_next and thus lets poll for
        // them manually.
        let map = future::poll_fn(|cx| Pin::new(&mut self.body).poll_trailers(cx))
            .await
            .map_err(|e| Status::from_error(Box::new(e)));

        map.map(|x| x.map(MetadataMap::from_headers))
    }

    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        if let State::ReadHeader = self.state {
            if self.buf.remaining() < HEADER_SIZE {
                return Ok(None);
            }

            let is_compressed = match self.buf.get_u8() {
                0 => false,
                1 => {
                    #[cfg(feature = "compression")]
                    {
                        if self.encoding.is_some() {
                            true
                        } else {
                            // https://grpc.github.io/grpc/core/md_doc_compression.html
                            // An ill-constructed message with its Compressed-Flag bit set but lacking a grpc-encoding
                            // entry different from identity in its metadata MUST fail with INTERNAL status,
                            // its associated description indicating the invalid Compressed-Flag condition.
                            return Err(Status::new(Code::Internal, "protocol error: received message with compressed-flag but no grpc-encoding was specified"));
                        }
                    }
                    #[cfg(not(feature = "compression"))]
                    {
                        return Err(Status::new(
                            Code::Unimplemented,
                            "Message compressed, compression support not enabled.".to_string(),
                        ));
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
            self.buf.reserve(len);

            self.state = State::ReadBody {
                compression: is_compressed,
                len,
            }
        }

        if let State::ReadBody { len, compression } = &self.state {
            // if we haven't read enough of the message then return and keep
            // reading
            if self.buf.remaining() < *len || self.buf.len() < *len {
                return Ok(None);
            }

            let decoding_result = if *compression {
                #[cfg(feature = "compression")]
                {
                    self.decompress_buf.clear();

                    if let Err(err) = decompress(
                        self.encoding.unwrap_or_else(|| {
                            // SAFETY: The check while in State::ReadHeader would already have returned Code::Internal
                            unreachable!("message was compressed but `Streaming.encoding` was `None`. This is a bug in Tonic. Please file an issue")
                        }),
                        &mut self.buf,
                        &mut self.decompress_buf,
                        *len,
                    ) {
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
                    self.decoder.decode(&mut DecodeBuf::new(
                        &mut self.decompress_buf,
                        decompressed_len,
                    ))
                }

                #[cfg(not(feature = "compression"))]
                unreachable!("should not take this branch if compression is disabled")
            } else {
                self.decoder
                    .decode(&mut DecodeBuf::new(&mut self.buf, *len))
            };

            return match decoding_result {
                Ok(Some(msg)) => {
                    self.state = State::ReadHeader;
                    Ok(Some(msg))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            };
        }

        Ok(None)
    }
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let State::Error = &self.state {
                return Poll::Ready(None);
            }

            // FIXME: implement the ability to poll trailers when we _know_ that
            // the consumer of this stream will only poll for the first message.
            // This means we skip the poll_trailers step.
            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            let chunk = match ready!(Pin::new(&mut self.body).poll_data(cx)) {
                Some(Ok(d)) => Some(d),
                Some(Err(e)) => {
                    let _ = std::mem::replace(&mut self.state, State::Error);
                    let err: crate::Error = e.into();
                    debug!("decoder inner stream error: {:?}", err);
                    let status = Status::from_error(err);
                    return Poll::Ready(Some(Err(status)));
                }
                None => None,
            };

            if let Some(data) = chunk {
                self.buf.put(data);
            } else {
                // FIXME: improve buf usage.
                if self.buf.has_remaining() {
                    trace!("unexpected EOF decoding stream");
                    return Poll::Ready(Some(Err(Status::new(
                        Code::Internal,
                        "Unexpected EOF decoding stream.".to_string(),
                    ))));
                } else {
                    break;
                }
            }
        }

        if let Direction::Response(status) = self.direction {
            match ready!(Pin::new(&mut self.body).poll_trailers(cx)) {
                Ok(trailer) => {
                    if let Err(e) = crate::status::infer_grpc_status(trailer.as_ref(), status) {
                        if let Some(e) = e {
                            return Some(Err(e)).into();
                        } else {
                            return Poll::Ready(None);
                        }
                    } else {
                        self.trailers = trailer.map(MetadataMap::from_headers);
                    }
                }
                Err(e) => {
                    let err: crate::Error = e.into();
                    debug!("decoder inner trailers error: {:?}", err);
                    let status = Status::from_error(err);
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

#[cfg(test)]
static_assertions::assert_impl_all!(Streaming<()>: Send);
