use crate::codec::compression::{get_codec, Compressor};
use crate::codec::serialization::{Deserialize, Serialize};
use crate::server::call::message_wrapper::CompressionEncoding;
use crate::server::call::{
    HandlerCallOptions, Incoming, Lazy, Outgoing, StreamingRequest, StreamingResponseWriter,
};
use crate::server::message::AsMut;

use crate::server::method_handler::{
    CodecRespB, GenericByteStreamMethodHandler, MessageStreamHandler,
};
use crate::server::stream::{PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter};
use crate::status::StatusCode;
use crate::Status;
use bytes::{Buf, Bytes, BytesMut};
use std::marker::PhantomData;

use std::sync::Arc;

const DEFAULT_DECOMPRESS_BUFFER_SIZE: usize = 8192;

/// A codec that adapts a GenericByteStreamMethodHandler to a MessageStreamHandler.
pub struct CodecMessageStreamHandler<H, Req, Resp> {
    inner: H,
    // Use fn(Req, Resp) to avoid imposing Send/Sync bounds on Req/Resp for the struct itself
    _pd: PhantomData<fn(Req, Resp)>,
}

impl<H, Req, Resp> CodecMessageStreamHandler<H, Req, Resp> {
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            _pd: PhantomData,
        }
    }
}

impl<H, Req, Resp> GenericByteStreamMethodHandler for CodecMessageStreamHandler<H, Req, Resp>
where
    H: MessageStreamHandler<Req = Req, Resp = Resp> + Send + Sync,
    Req: Send + AsMut + Deserialize + Default + 'static,
    Resp: Send + AsMut + Serialize + Default + 'static,
    H::ResponseHolder: 'static,
    for<'a> <Resp as AsMut>::Mut<'a>: Send + Serialize,
    for<'a> <Req as AsMut>::Mut<'a>: Send + Deserialize,
{
    type RespB = CodecRespB;

    async fn call<ReqB, P, W>(
        &self,
        options: HandlerCallOptions,
        req: StreamingRequest<P>,
        resp_writer: W,
    ) -> Result<(), Status>
    where
        ReqB: Buf + Send,
        P: PushStreamProducer<Item = Incoming<ReqB>> + Send + 'static,
        W: StreamingResponseWriter<Self::RespB> + Send,
        <W as StreamingResponseWriter<Self::RespB>>::MessageWriter: 'static,
    {
        // 1. Transform Request Stream: RawMessage -> Lazy<Req>
        let (metadata, raw_stream) = req.into_parts();

        // Resolve Decompressor
        let decompressor = if let Some(encoding) = metadata.encoding() {
            Some(get_codec(encoding).ok_or_else(|| {
                Status::new(
                    StatusCode::Unimplemented,
                    format!("compression encoding {} not found", encoding),
                )
            })?)
        } else {
            None
        };

        let typed_req_stream = PushStream::new(DecompressProducer {
            inner: raw_stream.into_inner(),
            decompressor,
            _pd: PhantomData,
        });

        // 2. Prepare Response Writer
        let compressor = options.compression_encoding.as_ref().and_then(|name| {
            if metadata.accept_encodings().any(|a| a == name) {
                get_codec(name)
            } else {
                None
            }
        });

        let typed_resp_writer = CodecResponseWriter::<_, H::ResponseHolder, Resp> {
            inner: resp_writer,
            compressor,
            _pd: PhantomData,
        };

        // 3. Call Inner Handler
        self.inner
            .call(
                options,
                StreamingRequest::new(typed_req_stream, metadata),
                typed_resp_writer,
            )
            .await
    }
}

// TODO: change the traits to eliminate this enum
pub enum Payload<B> {
    Raw(B),
    Decompressed(Bytes),
}

impl<B: Buf> Buf for Payload<B> {
    fn remaining(&self) -> usize {
        match self {
            Payload::Raw(b) => b.remaining(),
            Payload::Decompressed(b) => b.remaining(),
        }
    }

    fn chunk(&self) -> &[u8] {
        match self {
            Payload::Raw(b) => b.chunk(),
            Payload::Decompressed(b) => b.chunk(),
        }
    }

    fn advance(&mut self, cnt: usize) {
        match self {
            Payload::Raw(b) => b.advance(cnt),
            Payload::Decompressed(b) => b.advance(cnt),
        }
    }
}

struct DecompressProducer<P, Req> {
    inner: P,
    decompressor: Option<Arc<dyn Compressor>>,
    _pd: PhantomData<Req>,
}

impl<P, Req, B> PushStreamProducer for DecompressProducer<P, Req>
where
    P: PushStreamProducer<Item = Incoming<B>> + Send,
    B: Buf + Send,
    Req: Deserialize + Default + AsMut + Send,
    for<'a> <Req as AsMut>::Mut<'a>: Send + Deserialize,
{
    type Item = CodecLazy<Req, Payload<B>>;

    async fn produce(
        self,
        writer: PushStreamWriter<impl PushStreamConsumer<Self::Item>>,
    ) -> Result<(), Status> {
        let consumer = DecompressConsumer {
            inner: writer,
            decompressor: self.decompressor,
            // TODO: make the buffer capacity configurable from RpcOptions across the board.
            decompress_buf: BytesMut::with_capacity(DEFAULT_DECOMPRESS_BUFFER_SIZE),
            _pd: PhantomData,
        };
        self.inner.produce(PushStreamWriter::new(consumer)).await
    }
}

struct DecompressConsumer<C, Req> {
    inner: C,
    decompressor: Option<Arc<dyn Compressor>>,
    decompress_buf: BytesMut,
    _pd: PhantomData<Req>,
}

impl<C, Req, B> PushStreamConsumer<Incoming<B>> for DecompressConsumer<C, Req>
where
    C: PushStreamConsumer<CodecLazy<Req, Payload<B>>> + Send,
    B: Buf + Send,
    Req: Send,
{
    async fn write(&mut self, mut raw_msg: Incoming<B>) -> Result<(), Status> {
        let payload = if let Some(decompressor) = &self.decompressor {
            self.decompress_buf.clear();
            decompressor
                .decompress(&mut raw_msg.message_bytes, &mut self.decompress_buf)
                .map_err(|e| {
                    Status::new(StatusCode::Internal, format!("decompression error: {}", e))
                })?;
            Payload::Decompressed(
                self.decompress_buf
                    .split_to(self.decompress_buf.len())
                    .freeze(),
            )
        } else {
            Payload::Raw(raw_msg.message_bytes)
        };

        let decoded_msg = Incoming {
            message_bytes: payload,
            options: raw_msg.options,
        };

        let lazy = CodecLazy {
            raw_msg: decoded_msg,
            _pd: PhantomData,
        };

        self.inner.write(lazy).await
    }
}

// TODO(sauravzg): Consider supporting buffering here
// or in the transport layer due to hyper/H2 limitations
// to avoid writing very small messages.
struct CodecResponseWriter<W, Holder, Resp> {
    inner: W,
    compressor: Option<Arc<dyn Compressor>>,
    _pd: PhantomData<(Holder, Resp)>,
}

impl<W, Holder, Resp> StreamingResponseWriter<Outgoing<Holder>>
    for CodecResponseWriter<W, Holder, Resp>
where
    W: StreamingResponseWriter<CodecRespB> + Send,
    Holder: crate::server::method_handler::message_allocator::RpcResponseHolder<Resp> + Send,
    Resp: AsMut + Send,
    for<'a> <Resp as AsMut>::Mut<'a>: Send + Serialize,
{
    type MessageWriter = CodecResponseBodyWriter<W::MessageWriter, Holder, Resp>;
    type TrailerWriter = W::TrailerWriter;

    async fn send_initial_metadata(
        self,
        metadata: crate::server::call::Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
        let (inner_body_writer, trailer_writer) =
            self.inner.send_initial_metadata(metadata).await?;
        Ok((
            CodecResponseBodyWriter {
                inner: inner_body_writer,
                compressor: self.compressor,
                // TODO: make the buffer capacity configurable from RpcOptions across the board.
                buf: BytesMut::with_capacity(DEFAULT_DECOMPRESS_BUFFER_SIZE),
                _pd: PhantomData,
            },
            trailer_writer,
        ))
    }
}

struct CodecResponseBodyWriter<W, Holder, Resp> {
    inner: W,
    compressor: Option<Arc<dyn Compressor>>,
    buf: BytesMut,
    _pd: PhantomData<(Holder, Resp)>,
}

impl<W, Holder, Resp> PushStreamConsumer<Outgoing<Holder>>
    for CodecResponseBodyWriter<W, Holder, Resp>
where
    W: PushStreamConsumer<CodecRespB> + Send,
    Holder: crate::server::method_handler::message_allocator::RpcResponseHolder<Resp> + Send,
    Resp: AsMut + Send,
    for<'a> <Resp as AsMut>::Mut<'a>: Send + Serialize,
{
    async fn write(&mut self, item: Outgoing<Holder>) -> Result<(), Status> {
        let Outgoing {
            mut message,
            options,
        } = item;

        let resp_mut = message.get_response_mut();
        if let Err(e) = resp_mut.serialize(&mut self.buf) {
            return Err(Status::new(
                StatusCode::Internal,
                format!("serialization error: {:?}", e),
            ));
        }

        let message_compression = options.as_ref().map(|o| o.compression).unwrap_or_default();
        match (message_compression, &self.compressor) {
            (CompressionEncoding::Inherit | CompressionEncoding::Enabled, Some(comp)) => {
                // Split off the serialized bytes. `self.buf` is now empty but retains remaining capacity.
                let mut input = self.buf.split_to(self.buf.len());

                if let Err(e) = comp.compress(&mut input, &mut self.buf) {
                    return Err(Status::new(
                        StatusCode::Internal,
                        format!("compression error: {}", e),
                    ));
                }
                let bytes = self.buf.split_to(self.buf.len()).freeze();
                self.inner.write(bytes).await
            }
            _ => {
                let bytes = self.buf.split_to(self.buf.len()).freeze();
                self.inner.write(bytes).await
            }
        }
    }
}

pub struct CodecLazy<Req, B> {
    raw_msg: Incoming<B>,
    // TODO(sauravzg): Investigate replacing the Enum payload design with full static dispatch inside stream items (or evaluating how RecvStream inherently sidesteps this completely).
    _pd: PhantomData<Req>,
}

impl<Req, B> Lazy<Req> for CodecLazy<Req, B>
where
    Req: Deserialize + Default + AsMut + Send,
    B: Buf + Send,
    for<'a> <Req as AsMut>::Mut<'a>: Send + Deserialize,
{
    async fn resolve(self, mut dest: <Req as AsMut>::Mut<'_>) -> Result<(), Status> {
        let mut raw_msg = self.raw_msg;
        // Since decompression is handled eagerly by `DecompressConsumer`, we only deserialize!
        dest.deserialize(&mut raw_msg.message_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::send_future::SendFuture;
    use crate::server::call::metadata_writer::TrailingMetadataWriter;
    use crate::server::call::{Metadata, StreamingRequest, StreamingResponseWriter};
    use crate::server::method_handler::{HeapResponseHolder, MessageStreamHandler};
    use crate::server::stream::{
        PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter,
    };
    use crate::Status;
    use bytes::{Buf, Bytes, BytesMut};
    use protobuf_well_known_types::Timestamp;
    use tokio::sync::mpsc;

    struct MockMessageStreamHandler {
        expected_reqs: Vec<Timestamp>,
        resps_to_return: Vec<Outgoing<Timestamp>>,
    }

    impl MessageStreamHandler for MockMessageStreamHandler {
        type Req = Timestamp;
        type Resp = Timestamp;
        type ResponseHolder = HeapResponseHolder<Timestamp>;

        async fn call<P, W, L>(
            &self,
            _options: HandlerCallOptions,
            req: StreamingRequest<P>,
            writer: W,
        ) -> Result<(), Status>
        where
            P: PushStreamProducer<Item = L> + Send,
            W: StreamingResponseWriter<Outgoing<Self::ResponseHolder>> + Send,
            L: Lazy<Timestamp>,
        {
            let expected_reqs = self.expected_reqs.clone();

            let (_, stream) = req.into_parts();
            // We need to consume the stream to check expectations
            let producer = stream.into_inner();
            let (tx, mut rx) = mpsc::channel(10);
            let consumer = MockConsumer { tx };
            let stream_writer = PushStreamWriter::new(consumer);

            producer.produce(stream_writer).await?;

            let mut received = Vec::new();
            while let Some(lazy) = rx.recv().await {
                let mut msg = Timestamp::default();
                lazy.resolve(msg.as_mut()).make_send().await?;
                received.push(msg);
            }
            // assert_eq!(received, expected_reqs);

            let (mut msg_writer, trailer_writer) =
                writer.send_initial_metadata(Metadata::default()).await?;

            for resp_val in &self.resps_to_return {
                // We need to create a holder
                let holder = HeapResponseHolder::new(resp_val.message.clone());
                let mut outgoing = Outgoing::new(holder);
                outgoing.options = resp_val.options;
                msg_writer.write(outgoing).await?;
            }

            trailer_writer
                .send_trailing_metadata(Metadata::default())
                .await?;

            Ok(())
        }
    }

    struct MockConsumer<L> {
        tx: mpsc::Sender<L>,
    }

    impl<L: Send> crate::server::stream::PushStreamConsumer<L> for MockConsumer<L> {
        async fn write(&mut self, item: L) -> Result<(), Status> {
            self.tx.send(item).await.unwrap();
            Ok(())
        }
    }

    struct MockStreamingResponseWriter {
        tx: mpsc::Sender<Bytes>,
    }

    struct MockTrailerWriter;

    impl crate::server::call::metadata_writer::TrailingMetadataWriter for MockTrailerWriter {
        async fn send_trailing_metadata(self, _metadata: Metadata) -> Result<(), Status> {
            Ok(())
        }
    }

    impl StreamingResponseWriter<Bytes> for MockStreamingResponseWriter {
        type MessageWriter = MockConsumer<Bytes>;
        type TrailerWriter = MockTrailerWriter;

        async fn send_initial_metadata(
            self,
            _metadata: Metadata,
        ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
            Ok((MockConsumer { tx: self.tx }, MockTrailerWriter))
        }
    }

    struct MockProducer {
        messages: std::sync::Mutex<Vec<Incoming<Box<dyn Buf + Send>>>>,
    }

    impl PushStreamProducer for MockProducer {
        type Item = Incoming<Box<dyn Buf + Send>>;
        async fn produce(
            self,
            mut writer: PushStreamWriter<
                impl crate::server::stream::PushStreamConsumer<Self::Item>,
            >,
        ) -> Result<(), Status> {
            let messages = self.messages.into_inner().unwrap();
            for msg in messages {
                writer.write(msg).await?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_codec_message_stream_handler_success() {
        use protobuf::proto;

        let inner_method = MockMessageStreamHandler {
            expected_reqs: vec![
                proto!(Timestamp { seconds: 10 }),
                proto!(Timestamp { seconds: 20 }),
            ],
            resps_to_return: vec![
                Outgoing::new(proto!(Timestamp { seconds: 100 })),
                Outgoing::new(proto!(Timestamp { seconds: 200 })),
            ],
        };
        let handler = CodecMessageStreamHandler::new(inner_method);

        let mut req1 = BytesMut::new();
        proto!(Timestamp { seconds: 10 })
            .serialize(&mut req1)
            .unwrap();

        let mut req2 = BytesMut::new();
        proto!(Timestamp { seconds: 20 })
            .serialize(&mut req2)
            .unwrap();

        let raw_msgs = vec![
            Incoming {
                message_bytes: Box::new(req1.freeze()) as Box<dyn Buf + Send>,
                options: None,
            },
            Incoming {
                message_bytes: Box::new(req2.freeze()) as Box<dyn Buf + Send>,
                options: None,
            },
        ];

        let producer = MockProducer {
            messages: std::sync::Mutex::new(raw_msgs),
        };
        let stream = PushStream::new(producer);
        let req = StreamingRequest::new(stream, Metadata::default());

        let (tx_resp, mut rx_resp) = mpsc::channel(10);
        let resp_writer = MockStreamingResponseWriter { tx: tx_resp };

        let result = handler
            .call(HandlerCallOptions::default(), req, resp_writer)
            .await;

        assert!(result.is_ok());

        let resp1 = rx_resp.recv().await.unwrap();
        let mut buf1 = resp1;
        let mut ts1 = Timestamp::new();
        ts1.deserialize(&mut buf1).unwrap();
        assert_eq!(ts1.seconds(), 100);

        let resp2 = rx_resp.recv().await.unwrap();
        let mut buf2 = resp2;
        let mut ts2 = Timestamp::new();
        ts2.deserialize(&mut buf2).unwrap();
        assert_eq!(ts2.seconds(), 200);
    }
}
