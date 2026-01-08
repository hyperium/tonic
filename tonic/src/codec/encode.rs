use super::compression::{
    compress, CompressionEncoding, CompressionSettings, SingleMessageCompressionOverride,
};
use super::{BufferSettings, EncodeBuf, Encoder, DEFAULT_MAX_SEND_MESSAGE_SIZE, HEADER_SIZE};
use crate::Status;
use bytes::{BufMut, Bytes, BytesMut};
use http::HeaderMap;
use http_body::{Body, Frame};
use pin_project::pin_project;
#[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
use std::future::Future;
use std::{
    pin::Pin,
    task::{ready, Context, Poll},
};
#[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
use tokio::task::JoinHandle;
use tokio_stream::{adapters::Fuse, Stream, StreamExt};

#[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
#[derive(Debug)]
struct CompressionResult {
    compressed_data: BytesMut,
    was_compressed: bool,
    encoding: Option<CompressionEncoding>,
}

/// Combinator for efficient encoding of messages into reasonably sized buffers.
/// EncodedBytes encodes ready messages from its delegate stream into a BytesMut,
/// splitting off and yielding a buffer when either:
///  * The delegate stream polls as not ready, or
///  * The encoded buffer surpasses YIELD_THRESHOLD.
#[pin_project(project = EncodedBytesProj)]
#[derive(Debug)]
struct EncodedBytes<T, U> {
    #[pin]
    source: Fuse<U>,
    encoder: T,
    compression_encoding: Option<CompressionEncoding>,
    max_message_size: Option<usize>,
    buf: BytesMut,
    uncompression_buf: BytesMut,
    error: Option<Status>,
    #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
    #[pin]
    compression_task: Option<JoinHandle<Result<CompressionResult, Status>>>,
}

impl<T: Encoder, U: Stream> EncodedBytes<T, U> {
    fn new(
        encoder: T,
        source: U,
        compression_encoding: Option<CompressionEncoding>,
        compression_override: SingleMessageCompressionOverride,
        max_message_size: Option<usize>,
    ) -> Self {
        let buffer_settings = encoder.buffer_settings();
        let buf = BytesMut::with_capacity(buffer_settings.buffer_size);

        let compression_encoding =
            if compression_override == SingleMessageCompressionOverride::Disable {
                None
            } else {
                compression_encoding
            };

        let uncompression_buf = if compression_encoding.is_some() {
            BytesMut::with_capacity(buffer_settings.buffer_size)
        } else {
            BytesMut::new()
        };

        Self {
            source: source.fuse(),
            encoder,
            compression_encoding,
            max_message_size,
            buf,
            uncompression_buf,
            error: None,
            #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
            compression_task: None,
        }
    }
}

impl<T, U> EncodedBytes<T, U>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>>,
{
    fn encode_item_uncompressed(
        encoder: &mut T,
        item: T::Item,
        buf: &mut BytesMut,
        max_message_size: Option<usize>,
    ) -> Result<(), Status> {
        let offset = buf.len();
        buf.reserve(HEADER_SIZE);
        unsafe {
            buf.advance_mut(HEADER_SIZE);
        }

        if let Err(err) = encoder.encode(item, &mut EncodeBuf::new(buf)) {
            return Err(Status::internal(format!("Error encoding: {err}")));
        }

        finish_encoding(None, max_message_size, &mut buf[offset..])
    }

    /// Process the next item from the stream
    /// Returns true if we should spawn a blocking task (sets up compression_task)
    /// Returns false if item was processed inline
    fn process_next_item(
        encoder: &mut T,
        item: T::Item,
        buf: &mut BytesMut,
        uncompression_buf: &mut BytesMut,
        compression_encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
        #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
        compression_task: &mut Pin<
            &mut Option<JoinHandle<Result<CompressionResult, Status>>>,
        >,
        buffer_settings: &BufferSettings,
    ) -> Result<bool, Status> {
        let compression_settings = compression_encoding
            .map(|encoding| CompressionSettings::new(encoding, buffer_settings.buffer_size));

        if let Some(settings) = compression_settings {
            uncompression_buf.clear();
            if let Err(err) = encoder.encode(item, &mut EncodeBuf::new(uncompression_buf)) {
                return Err(Status::internal(format!("Error encoding: {err}")));
            }

            let uncompressed_len = uncompression_buf.len();

            // Check if we should use spawn_blocking (only when tokio is available)
            #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
            if let Some(spawn_threshold) = settings.spawn_blocking_threshold {
                if uncompressed_len >= spawn_threshold
                    && uncompressed_len >= settings.compression_threshold
                {
                    let data_to_compress = uncompression_buf.split().freeze();

                    let task = tokio::task::spawn_blocking(move || {
                        compress_blocking(data_to_compress, settings)
                    });

                    compression_task.set(Some(task));
                    return Ok(true);
                }
            }

            compress_and_encode_item(
                buf,
                uncompression_buf,
                settings,
                max_message_size,
                uncompressed_len,
            )?;
        } else {
            Self::encode_item_uncompressed(encoder, item, buf, max_message_size)?;
        }

        Ok(false)
    }

    #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
    fn poll_compression_task(
        compression_task: &mut Pin<&mut Option<JoinHandle<Result<CompressionResult, Status>>>>,
        buf: &mut BytesMut,
        max_message_size: Option<usize>,
        buffer_settings: &BufferSettings,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Status>>> {
        if let Some(task) = compression_task.as_mut().as_pin_mut() {
            match Future::poll(task, cx) {
                Poll::Ready(Ok(Ok(result))) => {
                    compression_task.set(None);

                    buf.reserve(HEADER_SIZE + result.compressed_data.len());
                    let offset = buf.len();

                    unsafe {
                        buf.advance_mut(HEADER_SIZE);
                    }

                    buf.extend_from_slice(&result.compressed_data);

                    let final_compression = if result.was_compressed {
                        result.encoding
                    } else {
                        None
                    };

                    if let Err(status) =
                        finish_encoding(final_compression, max_message_size, &mut buf[offset..])
                    {
                        return Poll::Ready(Some(Err(status)));
                    }

                    if buf.len() >= buffer_settings.yield_threshold {
                        return Poll::Ready(Some(Ok(buf.split_to(buf.len()).freeze())));
                    }
                    Poll::Ready(None)
                }
                Poll::Ready(Ok(Err(status))) => {
                    compression_task.set(None);
                    Poll::Ready(Some(Err(status)))
                }
                Poll::Ready(Err(_)) => {
                    compression_task.set(None);
                    Poll::Ready(Some(Err(Status::internal("compression task panicked"))))
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Ready(None)
        }
    }
}

impl<T, U> Stream for EncodedBytes<T, U>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>>,
{
    type Item = Result<Bytes, Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let EncodedBytesProj {
            mut source,
            encoder,
            compression_encoding,
            max_message_size,
            buf,
            uncompression_buf,
            error,
            #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
            mut compression_task,
        } = self.project();
        let buffer_settings = encoder.buffer_settings();

        if let Some(status) = error.take() {
            return Poll::Ready(Some(Err(status)));
        }

        // Check if we have an in-flight compression task
        #[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
        {
            match Self::poll_compression_task(
                &mut compression_task,
                buf,
                *max_message_size,
                &buffer_settings,
                cx,
            ) {
                Poll::Ready(Some(result)) => return Poll::Ready(Some(result)),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    // Task completed, continue processing
                }
            }
        }

        loop {
            match source.as_mut().poll_next(cx) {
                Poll::Pending if buf.is_empty() => {
                    return Poll::Pending;
                }
                Poll::Ready(None) if buf.is_empty() => {
                    return Poll::Ready(None);
                }
                Poll::Pending | Poll::Ready(None) => {
                    return Poll::Ready(Some(Ok(buf.split_to(buf.len()).freeze())));
                }
                Poll::Ready(Some(Ok(item))) => {
                    match Self::process_next_item(
                        encoder,
                        item,
                        buf,
                        uncompression_buf,
                        *compression_encoding,
                        *max_message_size,
                        #[cfg(any(
                            feature = "transport",
                            feature = "channel",
                            feature = "server"
                        ))]
                        &mut compression_task,
                        &buffer_settings,
                    ) {
                        Ok(true) => {
                            #[cfg(any(
                                feature = "transport",
                                feature = "channel",
                                feature = "server"
                            ))]
                            {
                                // We just spawned/armed the blocking compression task.
                                // Poll it once right away so it can capture our waker.
                                match Self::poll_compression_task(
                                    &mut compression_task,
                                    buf,
                                    *max_message_size,
                                    &buffer_settings,
                                    cx,
                                ) {
                                    Poll::Ready(Some(result)) => {
                                        return Poll::Ready(Some(result));
                                    }
                                    Poll::Ready(None) => {
                                        if buf.len() >= buffer_settings.yield_threshold {
                                            return Poll::Ready(Some(Ok(buf
                                                .split_to(buf.len())
                                                .freeze())));
                                        }
                                    }
                                    Poll::Pending => {
                                        return Poll::Pending;
                                    }
                                }
                            }
                            #[cfg(not(any(
                                feature = "transport",
                                feature = "channel",
                                feature = "server"
                            )))]
                            {
                                // This shouldn't happen when tokio is not available
                                unreachable!("spawn_blocking returned true without tokio")
                            }
                        }
                        Ok(false) => {
                            if buf.len() >= buffer_settings.yield_threshold {
                                return Poll::Ready(Some(Ok(buf.split_to(buf.len()).freeze())));
                            }
                        }
                        Err(status) => {
                            return Poll::Ready(Some(Err(status)));
                        }
                    }
                }
                Poll::Ready(Some(Err(status))) => {
                    if buf.is_empty() {
                        return Poll::Ready(Some(Err(status)));
                    }
                    *error = Some(status);
                    return Poll::Ready(Some(Ok(buf.split_to(buf.len()).freeze())));
                }
            }
        }
    }
}

/// Compress data in a blocking task (called via spawn_blocking)
#[cfg(any(feature = "transport", feature = "channel", feature = "server"))]
fn compress_blocking(
    data: Bytes,
    settings: CompressionSettings,
) -> Result<CompressionResult, Status> {
    let uncompressed_len = data.len();
    let mut uncompression_buf = BytesMut::from(data.as_ref());
    let mut compressed_buf = BytesMut::new();

    compress(
        settings,
        &mut uncompression_buf,
        &mut compressed_buf,
        uncompressed_len,
    )
    .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;

    Ok(CompressionResult {
        compressed_data: compressed_buf,
        was_compressed: true,
        encoding: Some(settings.encoding),
    })
}

/// Compress and encode an already-serialized item inline (without spawn_blocking)
fn compress_and_encode_item(
    buf: &mut BytesMut,
    uncompression_buf: &mut BytesMut,
    settings: CompressionSettings,
    max_message_size: Option<usize>,
    uncompressed_len: usize,
) -> Result<(), Status> {
    let offset = buf.len();

    buf.reserve(HEADER_SIZE);
    unsafe {
        buf.advance_mut(HEADER_SIZE);
    }

    let mut was_compressed = false;

    if uncompressed_len >= settings.compression_threshold {
        compress(settings, uncompression_buf, buf, uncompressed_len)
            .map_err(|err| Status::internal(format!("Error compressing: {err}")))?;
        was_compressed = true;
    } else {
        buf.reserve(uncompressed_len);
        buf.extend_from_slice(&uncompression_buf[..]);
    }

    // now that we know length, we can write the header
    let final_compression = if was_compressed {
        Some(settings.encoding)
    } else {
        None
    };
    finish_encoding(final_compression, max_message_size, &mut buf[offset..])
}

fn finish_encoding(
    compression_encoding: Option<CompressionEncoding>,
    max_message_size: Option<usize>,
    buf: &mut [u8],
) -> Result<(), Status> {
    let len = buf.len() - HEADER_SIZE;
    let limit = max_message_size.unwrap_or(DEFAULT_MAX_SEND_MESSAGE_SIZE);
    if len > limit {
        return Err(Status::out_of_range(format!(
            "Error, encoded message length too large: found {len} bytes, the limit is: {limit} bytes"
        )));
    }

    if len > u32::MAX as usize {
        return Err(Status::resource_exhausted(format!(
            "Cannot return body with more than 4GB of data but got {len} bytes"
        )));
    }
    {
        let mut buf = &mut buf[..HEADER_SIZE];
        buf.put_u8(compression_encoding.is_some() as u8);
        buf.put_u32(len as u32);
    }

    Ok(())
}

#[derive(Debug)]
enum Role {
    Client,
    Server,
}

/// A specialized implementation of [Body] for encoding [Result<Bytes, Status>].
#[pin_project]
#[derive(Debug)]
pub struct EncodeBody<T, U> {
    #[pin]
    inner: EncodedBytes<T, U>,
    state: EncodeState,
}

#[derive(Debug)]
struct EncodeState {
    error: Option<Status>,
    role: Role,
    is_end_stream: bool,
}

impl<T: Encoder, U: Stream> EncodeBody<T, U> {
    /// Turns a stream of grpc messages into [EncodeBody] which is used by grpc clients for
    /// turning the messages into http frames for sending over the network.
    pub fn new_client(
        encoder: T,
        source: U,
        compression_encoding: Option<CompressionEncoding>,
        max_message_size: Option<usize>,
    ) -> Self {
        Self {
            inner: EncodedBytes::new(
                encoder,
                source,
                compression_encoding,
                SingleMessageCompressionOverride::default(),
                max_message_size,
            ),
            state: EncodeState {
                error: None,
                role: Role::Client,
                is_end_stream: false,
            },
        }
    }

    /// Turns a stream of grpc results (message or error status) into [EncodeBody] which is used by grpc
    /// servers for turning the messages into http frames for sending over the network.
    pub fn new_server(
        encoder: T,
        source: U,
        compression_encoding: Option<CompressionEncoding>,
        compression_override: SingleMessageCompressionOverride,
        max_message_size: Option<usize>,
    ) -> Self {
        Self {
            inner: EncodedBytes::new(
                encoder,
                source,
                compression_encoding,
                compression_override,
                max_message_size,
            ),
            state: EncodeState {
                error: None,
                role: Role::Server,
                is_end_stream: false,
            },
        }
    }
}

impl EncodeState {
    fn trailers(&mut self) -> Option<Result<HeaderMap, Status>> {
        match self.role {
            Role::Client => None,
            Role::Server => {
                if self.is_end_stream {
                    return None;
                }

                self.is_end_stream = true;
                let status = if let Some(status) = self.error.take() {
                    status
                } else {
                    Status::ok("")
                };
                Some(status.to_header_map())
            }
        }
    }
}

impl<T, U> Body for EncodeBody<T, U>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>>,
{
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.state.is_end_stream
    }

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let self_proj = self.project();
        match ready!(self_proj.inner.poll_next(cx)) {
            Some(Ok(d)) => Some(Ok(Frame::data(d))).into(),
            Some(Err(status)) => match self_proj.state.role {
                Role::Client => Some(Err(status)).into(),
                Role::Server => {
                    self_proj.state.is_end_stream = true;
                    Some(Ok(Frame::trailers(status.to_header_map()?))).into()
                }
            },
            None => self_proj
                .state
                .trailers()
                .map(|t| t.map(Frame::trailers))
                .into(),
        }
    }
}
