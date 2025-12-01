use crate::send_future::SendFuture;

use crate::server::call::HandlerCallOptions;
use crate::server::call::{
    metadata_writer::TrailingMetadataWriter, Metadata, Outgoing, StreamingRequest,
    StreamingResponseWriter,
};
use crate::server::method_handler::MessageStreamHandler;
use crate::server::stream::{
    PushStreamConsumer, PushStreamExt, PushStreamProducer, PushStreamWriter,
};
use crate::server::BidiStreamingMethod;
use crate::Status;

use crate::server::call::Lazy;
use crate::server::message::AsMut;
use crate::server::method_handler::message_allocator::HeapResponseHolder;

/// Helper struct to adapt `PushStreamConsumer<Item=Outgoing<...>>` to `PushStreamConsumer<Item=Resp>`.
struct ResponseConsumerAdapter<W, Resp> {
    writer: W,
    _phantom: std::marker::PhantomData<Resp>,
}

impl<W, Resp> ResponseConsumerAdapter<W, Resp>
where
    W: PushStreamConsumer<Outgoing<HeapResponseHolder<Resp>>> + Send,
    Resp: Send + 'static,
{
    fn new(writer: W) -> Self {
        Self {
            writer,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<W, Resp> PushStreamConsumer<Resp> for ResponseConsumerAdapter<W, Resp>
where
    W: PushStreamConsumer<Outgoing<HeapResponseHolder<Resp>>> + Send,
    Resp: Send,
{
    async fn write(&mut self, item: Resp) -> Result<(), Status> {
        let holder = HeapResponseHolder::new(item);
        self.writer.write(Outgoing::new(holder)).await
    }
}

/// Adapter for `BidiStreamingMethod`.
pub struct BidiStreamingAdapter<T>(pub T);

impl<T, Req, Resp> MessageStreamHandler for BidiStreamingAdapter<T>
where
    T: BidiStreamingMethod<Req = Req, Resp = Resp> + Sync,
    Req: AsMut + Default + Send + 'static,
    Resp: AsMut + Default + Send + 'static,
{
    type Req = Req;
    type Resp = Resp;

    type ResponseHolder = HeapResponseHolder<Resp>;

    async fn call<P, W, L>(
        &self,
        _options: HandlerCallOptions,
        req: StreamingRequest<P>,
        writer: W,
    ) -> Result<(), Status>
    where
        P: PushStreamProducer<Item = L> + Send + 'static,
        W: StreamingResponseWriter<Outgoing<Self::ResponseHolder>> + Send,
        L: Lazy<Req>,
        <W as StreamingResponseWriter<Outgoing<Self::ResponseHolder>>>::MessageWriter: 'static,
    {
        // 1. Send Initial Metadata
        let (msg_writer, trailer_writer) =
            writer.send_initial_metadata(Metadata::default()).await?;

        // 2. Adapt input stream (L -> Req)
        let (_, stream) = req.into_parts();
        let req_stream = stream.then(|lazy_req| async move {
            let mut req = Req::default();
            lazy_req.resolve(req.as_mut()).make_send().await?;
            Ok(req)
        });

        // 3. Call method
        {
            let adapter = ResponseConsumerAdapter::new(msg_writer);
            let stream_writer = PushStreamWriter::new(adapter);
            self.0
                .bidi_streaming(req_stream, stream_writer)
                .await
                .map_err(|s| s.into_status())?;
        }

        // 4. Send Trailers
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::call::metadata_writer::{InitialMetadataWriter, TrailingMetadataWriter};
    use crate::server::call::test_util::StreamingResponseImpl;
    use crate::server::stream::{
        PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter,
    };
    use crate::server::BidiStreamingMethod;
    use crate::Status;

    struct MockBidiStreamingMethod {
        resp_to_return: i32,
    }

    impl BidiStreamingMethod for MockBidiStreamingMethod {
        type Req = i32;
        type Resp = i32;
        async fn bidi_streaming<P, C>(
            &self,
            _req: PushStream<P>,
            writer: PushStreamWriter<C>,
        ) -> Result<(), crate::ServerStatus>
        where
            P: PushStreamProducer<Item = i32> + Send,
            C: PushStreamConsumer<i32> + Send,
        {
            Ok(())
        }
    }

    struct MockMetadataWriter {
        sent_initial: bool,
        sent_trailing: bool,
    }

    impl InitialMetadataWriter for MockMetadataWriter {
        async fn send_initial_metadata(mut self, _metadata: Metadata) -> Result<(), Status> {
            self.sent_initial = true;
            Ok(())
        }
    }

    impl TrailingMetadataWriter for MockMetadataWriter {
        async fn send_trailing_metadata(mut self, _metadata: Metadata) -> Result<(), Status> {
            self.sent_trailing = true;
            Ok(())
        }
    }

    struct MockProducer;
    impl PushStreamProducer for MockProducer {
        type Item = i32;
        async fn produce(
            self,
            _writer: PushStreamWriter<impl crate::server::stream::PushStreamConsumer<Self::Item>,
            >,
        ) -> Result<(), Status> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_bidi_streaming_adapter_v2_success() {
        use protobuf_well_known_types::Timestamp;

        struct MockBidiStreamingMethodV2;
        impl BidiStreamingMethod for MockBidiStreamingMethodV2 {
            type Req = Timestamp;
            type Resp = Timestamp;
            async fn bidi_streaming<P, C>(
                &self,
                _req: PushStream<P>,
                mut writer: PushStreamWriter<C>,
            ) -> Result<(), crate::ServerStatus>
            where
                P: PushStreamProducer<Item = Timestamp> + Send,
                C: PushStreamConsumer<Timestamp> + Send,
            {
                // Write one item
                let mut msg = Timestamp::new();
                msg.set_seconds(100);
                writer.write(msg).await.unwrap();
                Ok(())
            }
        }

        let method = MockBidiStreamingMethodV2;
        let adapter = BidiStreamingAdapter(method);

        // V2 expects Lazy<Req>
        struct MockLazy(Timestamp);
        impl Lazy<Timestamp> for MockLazy {
            async fn resolve(self, mut dest: <Timestamp as AsMut>::Mut<'_>) -> Result<(), Status> {
                dest.set_seconds(self.0.seconds());
                Ok(())
            }
        }

        struct MockLazyProducer;
        impl PushStreamProducer for MockLazyProducer {
            type Item = MockLazy;
            async fn produce(
                self,
                writer: PushStreamWriter<impl PushStreamConsumer<Self::Item>>,
            ) -> Result<(), Status> {
                // Just close
                Ok(())
            }
        }

        let producer = MockLazyProducer;
        let stream = PushStream::new(producer);
        let req = StreamingRequest::new(stream, Metadata::default());

        // Consumer for Outgoing<HeapResponseHolder<Timestamp>>
        struct MockV2Consumer;
        impl PushStreamConsumer<Outgoing<HeapResponseHolder<Timestamp>>> for MockV2Consumer {
            async fn write(
                &mut self,
                _item: Outgoing<HeapResponseHolder<Timestamp>>,
            ) -> Result<(), Status> {
                Ok(())
            }
        }

        let stream_writer = PushStreamWriter::new(MockV2Consumer);

        let initial_writer = MockMetadataWriter {
            sent_initial: false,
            sent_trailing: false,
        };
        let trailing_writer = MockMetadataWriter {
            sent_initial: false,
            sent_trailing: false,
        };
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let result = adapter
            .call(HandlerCallOptions::default(), req, writer)
            .await;

        assert!(result.is_ok());
    }
}
