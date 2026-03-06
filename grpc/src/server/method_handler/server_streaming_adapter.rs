use std::marker::PhantomData;

use crate::send_future::SendFuture;
use crate::server::call::Lazy;
use crate::server::call::{
    metadata_writer::TrailingMetadataWriter, HandlerCallOptions, Metadata, Outgoing,
    StreamingRequest, StreamingResponseWriter,
};
use crate::server::message::AsMut;
use crate::server::message::AsView;
use crate::server::method_handler::message_allocator::HeapResponseHolder;
use crate::server::method_handler::MessageStreamHandler;
use crate::server::stream::{
    PushStreamConsumer, PushStreamExt, PushStreamProducer, PushStreamWriter,
};
use crate::server::ServerStreamingMethod;
use crate::Status;

/// Adapter for `ServerStreamingMethod`.
pub struct ServerStreamingAdapter<T>(pub T);

struct ResponseMessageConsumer<W, Resp> {
    writer: W,
    _marker: PhantomData<fn(Resp) -> ()>,
}

impl<W, Resp> ResponseMessageConsumer<W, Resp> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            _marker: PhantomData,
        }
    }
}

impl<W, Resp> PushStreamConsumer<Resp> for ResponseMessageConsumer<W, Resp>
where
    W: PushStreamConsumer<Outgoing<HeapResponseHolder<Resp>>> + Send,
    Resp: Send,
{
    async fn write(&mut self, item: Resp) -> Result<(), Status> {
        self.writer
            .write(Outgoing::new(HeapResponseHolder::new(item)))
            .await
    }
}

struct SingleMessageStreamConsumer<'a, M, W> {
    method: &'a M,
    writer: Option<W>,
}

impl<'a, M, W> SingleMessageStreamConsumer<'a, M, W> {
    fn new(writer: W, method: &'a M) -> Self {
        Self {
            method,
            writer: Some(writer),
        }
    }
}

impl<'a, M, W> PushStreamConsumer<M::Req> for SingleMessageStreamConsumer<'a, M, W>
where
    M: ServerStreamingMethod + Sync,
    W: PushStreamConsumer<Outgoing<HeapResponseHolder<M::Resp>>> + Send + 'static,
    M::Resp: AsMut + Send + 'static,
    M::Req: AsMut + AsView + Send + 'static,
{
    async fn write(&mut self, req: M::Req) -> Result<(), Status> {
        let writer = self.writer.take().ok_or_else(|| {
            Status::new(
                crate::status::StatusCode::Internal,
                "Unary request must have exactly one message",
            )
        })?;
        // Resolve request.
        self.method
            .server_streaming(
                req.as_view(),
                PushStreamWriter::new(ResponseMessageConsumer::new(writer)),
            )
            .make_send()
            .await
            .map_err(|s| s.into_status())?;
        Ok(())
    }
}

impl<T, Req, Resp> MessageStreamHandler for ServerStreamingAdapter<T>
where
    T: ServerStreamingMethod<Req = Req, Resp = Resp> + Sync,
    Req: AsMut + AsView + Default + Send + 'static,
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
        req_stream
            .run(PushStreamWriter::new(SingleMessageStreamConsumer::new(
                msg_writer, &self.0,
            )))
            .await?;

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
    use crate::server::call::{Metadata, StreamingRequest};
    use crate::server::message::AsView;
    use crate::server::stream::{
        PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter,
    };
    use crate::server::ServerStreamingMethod;
    use crate::{ServerStatus, Status, StatusCode};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use protobuf_well_known_types::Timestamp;

    struct MockServerStreamingMethod {
        expected_req: i32,
    }

    impl ServerStreamingMethod for MockServerStreamingMethod {
        type Req = Timestamp;
        type Resp = i32;
        async fn server_streaming<C>(
            &self,
            req: <Timestamp as AsView>::View<'_>,
            _writer: PushStreamWriter<C>,
        ) -> Result<(), ServerStatus>
        where
            C: PushStreamConsumer<i32> + Send,
        {
            assert_eq!(req.seconds(), self.expected_req as i64);
            // We can write to writer here if we want, but for now just return Ok
            Ok(())
        }
    }

    #[derive(Clone)]
    struct MockMetadataWriter {
        sent_initial: Arc<AtomicBool>,
        sent_trailing: Arc<AtomicBool>,
    }

    impl InitialMetadataWriter for MockMetadataWriter {
        async fn send_initial_metadata(self, _metadata: Metadata) -> Result<(), Status> {
            self.sent_initial.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    impl TrailingMetadataWriter for MockMetadataWriter {
        async fn send_trailing_metadata(self, _metadata: Metadata) -> Result<(), Status> {
            self.sent_trailing.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct MockProducer {
        item: Timestamp,
    }
    impl PushStreamProducer for MockProducer {
        type Item = Timestamp;
        async fn produce(
            self,
            mut writer: PushStreamWriter<impl crate::server::stream::PushStreamConsumer<Self::Item>,
            >,
        ) -> Result<(), Status> {
            writer.write(self.item).await
        }
    }

    #[tokio::test]
    async fn test_server_streaming_adapter_v2_success() {
        struct MockServerStreamingMethodV2 {
            expected_req: Timestamp,
        }
        impl ServerStreamingMethod for MockServerStreamingMethodV2 {
            type Req = Timestamp;
            type Resp = Timestamp;
            async fn server_streaming<C>(
                &self,
                req: <Timestamp as AsView>::View<'_>,
                mut writer: PushStreamWriter<C>,
            ) -> Result<(), crate::ServerStatus>
            where
                C: PushStreamConsumer<Timestamp> + Send,
            {
                if req.seconds() != self.expected_req.seconds() {
                    return Err(ServerStatus::new(StatusCode::Internal, "req mismatch"));
                }
                // Need to convert View back to Owned or clone data if we want to write it back?
                // But we are writing `Timestamp` (owned).
                // View has accessors.
                let mut resp = Timestamp::new();
                resp.set_seconds(req.seconds());
                writer.write(resp).await.unwrap();
                Ok(())
            }
        }

        let mut expected = Timestamp::new();
        expected.set_seconds(42);
        let method = MockServerStreamingMethodV2 {
            expected_req: expected.clone(),
        };
        let adapter = ServerStreamingAdapter(method);

        struct MockLazy(Timestamp);
        impl Lazy<Timestamp> for MockLazy {
            async fn resolve(self, mut dest: <Timestamp as AsMut>::Mut<'_>) -> Result<(), Status> {
                dest.set_seconds(self.0.seconds());
                Ok(())
            }
        }

        struct MockLazyProducer {
            item: Timestamp,
        }
        impl PushStreamProducer for MockLazyProducer {
            type Item = MockLazy;
            async fn produce(
                self,
                mut writer: PushStreamWriter<impl PushStreamConsumer<Self::Item>>,
            ) -> Result<(), Status> {
                writer.write(MockLazy(self.item)).await
            }
        }

        let producer = MockLazyProducer {
            item: expected.clone(),
        };
        let stream = PushStream::new(producer);
        let req = StreamingRequest::new(stream, Metadata::default());

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

        let sent_initial = Arc::new(AtomicBool::new(false));
        let sent_trailing = Arc::new(AtomicBool::new(false));

        let initial_writer = MockMetadataWriter {
            sent_initial: sent_initial.clone(),
            sent_trailing: sent_trailing.clone(),
        };
        let trailing_writer = MockMetadataWriter {
            sent_initial: sent_initial.clone(),
            sent_trailing: sent_trailing.clone(),
        };

        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let result = adapter
            .call(HandlerCallOptions::default(), req, writer)
            .await;

        assert!(result.is_ok());
        assert!(sent_initial.load(Ordering::SeqCst));
        assert!(sent_trailing.load(Ordering::SeqCst));
    }
}
