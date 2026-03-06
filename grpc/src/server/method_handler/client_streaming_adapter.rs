use crate::send_future::SendFuture;
use crate::server::call::HandlerCallOptions;
use crate::server::call::{
    metadata_writer::TrailingMetadataWriter, Metadata, Outgoing, StreamingRequest,
    StreamingResponseWriter,
};
use crate::server::message::AsMut;
use crate::server::method_handler::MessageStreamHandler;
use crate::server::stream::{PushStreamConsumer, PushStreamExt, PushStreamProducer};
use crate::server::ClientStreamingMethod;
use crate::Status;

use crate::server::call::Lazy;
use crate::server::method_handler::message_allocator::HeapResponseHolder;

/// Adapter for `ClientStreamingMethod`.
pub struct ClientStreamingAdapter<T>(pub T);

impl<T, Req, Resp> MessageStreamHandler for ClientStreamingAdapter<T>
where
    T: ClientStreamingMethod<Req = Req, Resp = Resp> + Sync,
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
        let (mut msg_writer, trailer_writer) =
            writer.send_initial_metadata(Metadata::default()).await?;

        // 2. Adapt input stream (L -> Req)
        let (_, stream) = req.into_parts();
        let req_stream = stream.then(|lazy_req| async move {
            let mut req = Req::default();
            lazy_req.resolve(req.as_mut()).make_send().await?;
            Ok(req)
        });

        // 3. Call method
        let mut resp = Resp::default();
        self.0
            .client_streaming(req_stream, resp.as_mut())
            .make_send()
            .await
            .map_err(|s| s.into_status())?;

        // 4. Write response
        msg_writer
            .write(Outgoing::new(HeapResponseHolder::new(resp)))
            .await?;

        // 5. Send Trailers
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
    use crate::server::message::AsMut;
    use crate::server::stream::{
        PushStream, PushStreamConsumer, PushStreamProducer, PushStreamWriter,
    };
    use crate::server::ClientStreamingMethod;
    use crate::Status;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use protobuf_well_known_types::Timestamp;

    struct MockClientStreamingMethod {
        resp_to_return: i32,
    }

    impl ClientStreamingMethod for MockClientStreamingMethod {
        type Req = i32;
        type Resp = Timestamp;
        async fn client_streaming<P>(
            &self,
            req: PushStream<P>,
            mut resp: <Timestamp as AsMut>::Mut<'_>,
        ) -> Result<(), crate::ServerStatus>
        where
            P: PushStreamProducer<Item = i32> + Send,
        {
            let _ = req;
            resp.set_seconds(self.resp_to_return as i64);
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
    async fn test_client_streaming_adapter_v2_success() {
        struct MockClientStreamingMethodV2;
        impl ClientStreamingMethod for MockClientStreamingMethodV2 {
            type Req = Timestamp;
            type Resp = Timestamp;
            async fn client_streaming<P>(
                &self,
                req: PushStream<P>,
                mut resp: <Timestamp as AsMut>::Mut<'_>,
            ) -> Result<(), crate::ServerStatus>
            where
                P: PushStreamProducer<Item = Timestamp> + Send,
            {
                let _ = req;
                resp.set_seconds(100);
                Ok(())
            }
        }

        let method = MockClientStreamingMethodV2;
        let adapter = ClientStreamingAdapter(method);

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
                // Return 0 items, just close stream
                Ok(())
            }
        }

        let producer = MockLazyProducer;
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
