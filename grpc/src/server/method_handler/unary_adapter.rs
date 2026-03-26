use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::send_future::SendFuture;
use crate::server::call::{metadata_writer::TrailingMetadataWriter, HandlerCallOptions, Metadata};
use crate::server::message::{AsMut, AsView};
use crate::server::method_handler::{MessageStreamHandler, RpcRequestHolder};
use crate::server::stream::PushStreamWriter;
use crate::server::UnaryMethod;
use crate::Status;
use crate::{
    server::call::{Lazy, Outgoing, StreamingRequest, StreamingResponseWriter},
    server::method_handler::message_allocator::{RpcMessageAllocator, RpcMessageHolder},
    server::stream::{PushStreamConsumer, PushStreamProducer},
};

pub struct UnaryMethodAdapter<T, A> {
    method: T,
    allocator: A,
}

impl<T, A> UnaryMethodAdapter<T, A> {
    pub fn new(method: T, allocator: A) -> Self {
        Self { method, allocator }
    }
}

struct SingleMessageStreamConsumer<'a, M, W, Resp, Req, A, L> {
    method: &'a M,
    response_stream_writer: &'a mut W,
    allocator: &'a A,
    called: AtomicBool,
    _marker: PhantomData<fn(L, Req, Resp) -> ()>,
}

impl<'a, M, W, Resp, Req, A, L> SingleMessageStreamConsumer<'a, M, W, Resp, Req, A, L> {
    fn new(response_stream_writer: &'a mut W, method: &'a M, allocator: &'a A) -> Self {
        Self {
            response_stream_writer,
            called: AtomicBool::new(false),
            method,
            allocator,
            _marker: PhantomData,
        }
    }
}

impl<'a, M, W, Resp, Req, A, L> PushStreamConsumer<L>
    for SingleMessageStreamConsumer<'a, M, W, Resp, Req, A, L>
where
    M: UnaryMethod<Req = Req, Resp = Resp> + Sync,
    W: PushStreamConsumer<Outgoing<A::Holder>>,
    Resp: AsMut + Send,
    Req: AsMut + AsView + Send,
    A: RpcMessageAllocator<Req, Resp>,
    L: Lazy<Req>,
{
    async fn write(&mut self, item: L) -> Result<(), Status> {
        if self.called.swap(true, Ordering::SeqCst) {
            return Err(Status::new(
                crate::status::StatusCode::Internal,
                "Unary request must have exactly one message",
            ));
        }
        // Resolve request.
        let mut holder = self.allocator.allocate();
        item.resolve(holder.get_request_mut()).make_send().await?;

        let (req_view, resp_mut) = holder.get_request_view_and_response_mut();
        self.method
            .unary(req_view, resp_mut)
            .make_send()
            .await
            .map_err(|s| s.into_status())?;

        self.response_stream_writer
            .write(Outgoing::new(holder))
            .await?;
        Ok(())
    }
}

impl<T, Req, Resp, A> MessageStreamHandler for UnaryMethodAdapter<T, A>
where
    T: UnaryMethod<Req = Req, Resp = Resp> + Sync,
    Req: AsView + AsMut + Send,
    Resp: AsMut + Default + Send,
    A: RpcMessageAllocator<Req, Resp>,
{
    type Req = Req;
    type Resp = Resp;

    type ResponseHolder = A::Holder;

    /// Handles a streaming request.
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
        // Send Metadata
        let (metadata, stream) = req.into_parts();
        let (mut msg_writer, trailer_writer) =
            writer.send_initial_metadata(Metadata::default()).await?;

        // Call unary method and send response.
        stream
            .run(PushStreamWriter::new(SingleMessageStreamConsumer::new(
                &mut msg_writer,
                &self.method,
                &self.allocator,
            )))
            .await?;

        // Send Trailers
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::server::call::metadata_writer::InitialMetadataWriter;
    use crate::server::call::metadata_writer::TrailingMetadataWriter;
    use crate::server::call::{Metadata, Outgoing};
    use crate::server::message::{AsMut, AsView};
    use crate::server::method_handler::message_allocator::HeapMessageHolder;
    use crate::server::UnaryMethod;
    use crate::Status;

    use protobuf_well_known_types::Timestamp;

    struct MockUnaryMethod {
        expected_req: i32,
        resp_to_return: i32,
    }

    impl UnaryMethod for MockUnaryMethod {
        type Req = Timestamp;
        type Resp = Timestamp;
        async fn unary(
            &self,
            req: <Timestamp as AsView>::View<'_>,
            mut resp: <Timestamp as AsMut>::Mut<'_>,
        ) -> Result<(), crate::ServerStatus> {
            assert_eq!(req.seconds(), self.expected_req as i64);
            resp.set_seconds(self.resp_to_return as i64);
            Ok(())
        }
    }

    struct MockMetadataWriter {
        sent_initial: bool,
        sent_trailing: bool,
    }

    impl InitialMetadataWriter for MockMetadataWriter {
        async fn send_initial_metadata(self, _metadata: Metadata) -> Result<(), Status> {
            Ok(())
        }
    }

    impl TrailingMetadataWriter for MockMetadataWriter {
        async fn send_trailing_metadata(self, _metadata: Metadata) -> Result<(), Status> {
            Ok(())
        }
    }

    // Helper struct for Lazy implementation in tests
    struct LazyTimestamp(Timestamp);

    impl crate::server::call::Lazy<Timestamp> for LazyTimestamp {
        async fn resolve(self, mut target: <Timestamp as AsMut>::Mut<'_>) -> Result<(), Status> {
            target.set_seconds(self.0.seconds());
            target.set_nanos(self.0.nanos());
            Ok(())
        }
    }

    struct MockProducer {
        item: i32,
    }

    impl PushStreamProducer for MockProducer {
        type Item = LazyTimestamp;

        async fn produce(
            self,
            mut writer: PushStreamWriter<impl PushStreamConsumer<Self::Item>>,
        ) -> Result<(), Status> {
            let mut msg = Timestamp::new();
            msg.set_seconds(self.item as i64);
            let lazy = LazyTimestamp(msg);
            writer.write(lazy).await
        }
    }

    #[allow(clippy::type_complexity)]
    struct TestUnaryConsumer {
        items: Arc<Mutex<Vec<Outgoing<HeapMessageHolder<Timestamp, Timestamp>>>>>,
    }

    impl PushStreamConsumer<Outgoing<HeapMessageHolder<Timestamp, Timestamp>>> for TestUnaryConsumer {
        async fn write(
            &mut self,
            item: Outgoing<HeapMessageHolder<Timestamp, Timestamp>>,
        ) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_unary_method_adapter_v2_success() {
        use crate::server::call::test_util::StreamingResponseImpl;
        use crate::server::call::StreamingRequest;
        use crate::server::method_handler::message_allocator::HeapMessageAllocator;
        use crate::server::stream::{PushStream, PushStreamWriter};

        // Setup Method
        let method = MockUnaryMethod {
            expected_req: 42,
            resp_to_return: 100,
        };
        let allocator = HeapMessageAllocator::<Timestamp, Timestamp>::new();
        let adapter = UnaryMethodAdapter { method, allocator };

        // Setup Request
        let producer = MockProducer { item: 42 };
        let req = StreamingRequest::new(PushStream::new(producer), Metadata::default());

        // Setup Writer
        let items = Arc::new(Mutex::new(Vec::new()));
        let consumer = TestUnaryConsumer {
            items: items.clone(),
        };
        let stream_writer = PushStreamWriter::new(consumer);

        let initial_writer = MockMetadataWriter {
            sent_initial: false,
            sent_trailing: false,
        };
        let trailing_writer = MockMetadataWriter {
            sent_initial: false,
            sent_trailing: false,
        };
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        // Call
        let result = adapter
            .call(HandlerCallOptions::default(), req, writer)
            .await;

        assert!(result.is_ok());

        let items = items.lock().unwrap();
        assert_eq!(items.len(), 1);
    }
}
