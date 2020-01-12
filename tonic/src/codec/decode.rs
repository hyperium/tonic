use super::{DecodeBuf, Decoder};
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
    decoder: Box<dyn Decoder<Item = T, Error = Status> + Send + Sync + 'static>,
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
    ReadBody { compression: bool, len: usize },
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
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + Sync + 'static,
    {
        Self::new(decoder, body, Direction::Response(status_code))
    }

    pub(crate) fn new_empty<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + Sync + 'static,
    {
        Self::new(decoder, body, Direction::EmptyResponse)
    }

    #[doc(hidden)]
    pub fn new_request<B, D>(decoder: D, body: B) -> Self
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + Sync + 'static,
    {
        Self::new(decoder, body, Direction::Request)
    }

    fn new<B, D>(decoder: D, body: B, direction: Direction) -> Self
    where
        B: Body + Send + Sync + 'static,
        B::Error: Into<crate::Error>,
        D: Decoder<Item = T, Error = Status> + Send + Sync + 'static,
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
    /// # use tonic::{Streaming, Status, codec::Decoder};
    /// # use std::fmt::Debug;
    /// # async fn next_message_ex<T, D>(mut request: Streaming<T>) -> Result<(), Status>
    /// # where T: Debug,
    /// # D: Decoder<Item = T, Error = Status> + Send + Sync + 'static,
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
        if let State::ReadHeader = self.state {
            if self.buf.remaining() < 5 {
                return Ok(None);
            }

            let is_compressed = match self.buf.get_u8() {
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
            let len = self.buf.get_u32() as usize;

            self.state = State::ReadBody {
                compression: is_compressed,
                len,
            }
        }

        if let State::ReadBody { len, .. } = &self.state {
            // if we haven't read enough of the message then return and keep
            // reading
            if self.buf.remaining() < *len || self.buf.len() < *len {
                return Ok(None);
            }

            return match self
                .decoder
                .decode(&mut DecodeBuf::new(&mut self.buf, *len))
            {
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
            // FIXME: implement the ability to poll trailers when we _know_ that
            // the consumer of this stream will only poll for the first message.
            // This means we skip the poll_trailers step.
            match self.decode_chunk()? {
                Some(item) => return Poll::Ready(Some(Ok(item))),
                None => (),
            }

            let chunk = match ready!(Pin::new(&mut self.body).poll_data(cx)) {
                Some(Ok(d)) => Some(d),
                Some(Err(e)) => {
                    let err: crate::Error = e.into();
                    debug!("decoder inner stream error: {:?}", err);
                    let status = Status::from_error(&*err);
                    Err(status)?;
                    break;
                }
                None => None,
            };

            if let Some(data) = chunk {
                if data.remaining() > self.buf.remaining_mut() {
                    let amt = if data.remaining() > BUFFER_SIZE {
                        data.remaining()
                    } else {
                        BUFFER_SIZE
                    };

                    self.buf.reserve(amt);
                }

                self.buf.put(data);
            } else {
                // FIXME: improve buf usage.
                if self.buf.has_remaining() {
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

#[cfg(test)]
static_assertions::assert_impl_all!(Streaming<()>: Send, Sync);
