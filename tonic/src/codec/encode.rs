#[cfg(feature = "compression")]
use super::compression::{compress, CompressionEncoding, SingleMessageCompressionOverride};
use super::{EncodeBuf, Encoder, HEADER_SIZE};
use crate::{Code, Status};
use bytes::{BufMut, Bytes, BytesMut};
use futures_core::{Stream, TryStream};
use futures_util::{ready, StreamExt, TryStreamExt};
use http::HeaderMap;
use http_body::Body;
use pin_project::pin_project;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

pub(super) const BUFFER_SIZE: usize = 8 * 1024;

pub(crate) fn encode_server<T, U>(
    encoder: T,
    source: U,
    #[cfg(feature = "compression")] compression_encoding: Option<CompressionEncoding>,
    #[cfg(feature = "compression")] compression_override: SingleMessageCompressionOverride,
) -> EncodeBody<impl Stream<Item = Result<Bytes, Status>>>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>>,
{
    let stream = encode(
        encoder,
        source,
        #[cfg(feature = "compression")]
        compression_encoding,
        #[cfg(feature = "compression")]
        compression_override,
    )
    .into_stream();

    EncodeBody::new_server(stream)
}

pub(crate) fn encode_client<T, U>(
    encoder: T,
    source: U,
    #[cfg(feature = "compression")] compression_encoding: Option<CompressionEncoding>,
) -> EncodeBody<impl Stream<Item = Result<Bytes, Status>>>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = T::Item>,
{
    let stream = encode(
        encoder,
        source.map(Ok),
        #[cfg(feature = "compression")]
        compression_encoding,
        #[cfg(feature = "compression")]
        SingleMessageCompressionOverride::default(),
    )
    .into_stream();
    EncodeBody::new_client(stream)
}

fn encode<T, U>(
    mut encoder: T,
    source: U,
    #[cfg(feature = "compression")] compression_encoding: Option<CompressionEncoding>,
    #[cfg(feature = "compression")] compression_override: SingleMessageCompressionOverride,
) -> impl TryStream<Ok = Bytes, Error = Status>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>>,
{
    async_stream::stream! {
        let mut buf = BytesMut::with_capacity(BUFFER_SIZE);

        #[cfg(feature = "compression")]
        let (compression_enabled_for_stream, mut uncompression_buf) = match compression_encoding {
            Some(CompressionEncoding::Gzip) => (true, BytesMut::with_capacity(BUFFER_SIZE)),
            None => (false, BytesMut::new()),
        };

        #[cfg(feature = "compression")]
        let compress_item = compression_enabled_for_stream && compression_override == SingleMessageCompressionOverride::Inherit;

        #[cfg(not(feature = "compression"))]
        let compress_item = false;

        futures_util::pin_mut!(source);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    buf.reserve(HEADER_SIZE);
                    unsafe {
                        buf.advance_mut(HEADER_SIZE);
                    }

                    if compress_item {
                        #[cfg(feature = "compression")]
                        {
                            uncompression_buf.clear();

                            encoder.encode(item, &mut EncodeBuf::new(&mut uncompression_buf))
                                .map_err(|err| Status::internal(format!("Error encoding: {}", err)))?;

                            let uncompressed_len = uncompression_buf.len();

                            compress(
                                compression_encoding.unwrap(),
                                &mut uncompression_buf,
                                &mut buf,
                                uncompressed_len,
                            ).map_err(|err| Status::internal(format!("Error compressing: {}", err)))?;
                        }

                        #[cfg(not(feature = "compression"))]
                        unreachable!("compression disabled, should not take this branch");
                    } else {
                        encoder.encode(item, &mut EncodeBuf::new(&mut buf))
                            .map_err(|err| Status::internal(format!("Error encoding: {}", err)))?;
                    }

                    // now that we know length, we can write the header
                    let len = buf.len() - HEADER_SIZE;
                    assert!(len <= std::u32::MAX as usize);
                    {
                        let mut buf = &mut buf[..HEADER_SIZE];
                        buf.put_u8(compress_item as u8);
                        buf.put_u32(len as u32);
                    }

                    yield Ok(buf.split_to(len + HEADER_SIZE).freeze());
                },
                Some(Err(status)) => yield Err(status),
                None => break,
            }
        }
    }
}

#[derive(Debug)]
enum Role {
    Client,
    Server,
}

#[pin_project]
#[derive(Debug)]
pub(crate) struct EncodeBody<S> {
    #[pin]
    inner: S,
    error: Option<Status>,
    role: Role,
    is_end_stream: bool,
}

impl<S> EncodeBody<S>
where
    S: Stream<Item = Result<Bytes, Status>>,
{
    pub(crate) fn new_client(inner: S) -> Self {
        Self {
            inner,
            error: None,
            role: Role::Client,
            is_end_stream: false,
        }
    }

    pub(crate) fn new_server(inner: S) -> Self {
        Self {
            inner,
            error: None,
            role: Role::Server,
            is_end_stream: false,
        }
    }
}

impl<S> Body for EncodeBody<S>
where
    S: Stream<Item = Result<Bytes, Status>>,
{
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.is_end_stream
    }

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let mut self_proj = self.project();
        match ready!(self_proj.inner.try_poll_next_unpin(cx)) {
            Some(Ok(d)) => Some(Ok(d)).into(),
            Some(Err(status)) => match self_proj.role {
                Role::Client => Some(Err(status)).into(),
                Role::Server => {
                    *self_proj.error = Some(status);
                    None.into()
                }
            },
            None => None.into(),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Status>> {
        match self.role {
            Role::Client => Poll::Ready(Ok(None)),
            Role::Server => {
                let self_proj = self.project();

                if *self_proj.is_end_stream {
                    return Poll::Ready(Ok(None));
                }

                let status = if let Some(status) = self_proj.error.take() {
                    *self_proj.is_end_stream = true;
                    status
                } else {
                    Status::new(Code::Ok, "")
                };

                Poll::Ready(Ok(Some(status.to_header_map()?)))
            }
        }
    }
}
