use crate::server::call::metadata_writer::TrailingMetadataWriter;
use crate::server::call::Metadata;
use crate::server::stream::PushStreamConsumer;
use crate::Status;

/// A trait representing a gRPC response.
///
/// This trait enforces the correct state transitions:
/// 1. Send initial metadata -> transitions to capable writing of body and trailers.
#[trait_variant::make(Send)]
pub trait StreamingResponseWriter<T: Send>: Send {
    /// The message writer type.
    type MessageWriter: PushStreamConsumer<T> + Send;
    /// The trailer writer type.
    type TrailerWriter: TrailingMetadataWriter + Send;

    /// Sends initial metadata and returns the discrete message and trailer writer capabilities.
    async fn send_initial_metadata(
        self,
        metadata: Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::call::metadata_writer::{InitialMetadataWriter, TrailingMetadataWriter};
    use crate::server::call::test_util::StreamingResponseImpl;
    use crate::server::call::Metadata;
    use crate::server::stream::PushStreamWriter;
    use crate::Status;

    struct MockMetadataWriter;

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

    struct MockPushStreamConsumer;

    impl crate::server::stream::PushStreamConsumer<i32> for MockPushStreamConsumer {
        async fn write(&mut self, _item: i32) -> Result<(), Status> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_streaming_response_writer_flow() {
        let consumer = MockPushStreamConsumer;
        let stream_writer = PushStreamWriter::new(consumer);
        let initial_writer = MockMetadataWriter;
        let trailing_writer = MockMetadataWriter;
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        // 1. Send initial metadata
        let (mut msg_writer, trailer_writer) = writer
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();

        // 2. Write body
        msg_writer.write(1).await.unwrap();
        msg_writer.write(2).await.unwrap();

        // 3. Send trailing metadata
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await
            .unwrap();
    }

    struct Interceptor<T>(T);

    impl<T, Item> StreamingResponseWriter<Item> for Interceptor<T>
    where
        T: StreamingResponseWriter<Item> + Send,
        Item: Send,
    {
        type MessageWriter = InterceptorMessageWriter<T::MessageWriter>;
        type TrailerWriter = InterceptorTrailerWriter<T::TrailerWriter>;

        async fn send_initial_metadata(
            self,
            metadata: Metadata,
        ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
            // Intercept headers here
            let (msg, trl) = self.0.send_initial_metadata(metadata).await?;
            Ok((InterceptorMessageWriter(msg), InterceptorTrailerWriter(trl)))
        }
    }

    struct InterceptorMessageWriter<T>(T);

    impl<T, Item> PushStreamConsumer<Item> for InterceptorMessageWriter<T>
    where
        T: PushStreamConsumer<Item> + Send,
        Item: Send,
    {
        async fn write(&mut self, item: Item) -> Result<(), Status> {
            self.0.write(item).await
        }
    }

    struct InterceptorTrailerWriter<T>(T);

    impl<T> TrailingMetadataWriter for InterceptorTrailerWriter<T>
    where
        T: TrailingMetadataWriter + Send,
    {
        async fn send_trailing_metadata(self, trailers: Metadata) -> Result<(), Status> {
            self.0.send_trailing_metadata(trailers).await
        }
    }

    #[tokio::test]
    async fn test_interceptor_composition() {
        let consumer = MockPushStreamConsumer;
        let stream_writer = PushStreamWriter::new(consumer);
        let initial_writer = MockMetadataWriter;
        let trailing_writer = MockMetadataWriter;
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);
        let interceptor = Interceptor(writer);

        let (mut msg_writer, trailer_writer) = interceptor
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();
        msg_writer.write(1).await.unwrap();
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await
            .unwrap();
    }
}
