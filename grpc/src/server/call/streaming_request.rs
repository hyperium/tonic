use crate::server::call::Metadata;
use crate::server::stream::PushStream;

/// A GrpcRequest stream containing initial metadata and a request stream.
pub struct StreamingRequest<P> {
    stream: PushStream<P>,
    initial_metadata: Metadata,
}

impl<P> StreamingRequest<P> {
    /// Creates a new StreamingRequest.
    pub fn new(stream: PushStream<P>, initial_metadata: Metadata) -> Self {
        Self {
            stream,
            initial_metadata,
        }
    }

    /// Returns a reference to the initial metadata.
    pub fn initial_metadata(&self) -> &Metadata {
        &self.initial_metadata
    }

    /// Decomposes the request into its parts.
    pub fn into_parts(self) -> (Metadata, PushStream<P>) {
        (self.initial_metadata, self.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::call::Metadata;
    use crate::server::stream::PushStream;
    use crate::server::stream::PushStreamConsumer;
    use crate::server::stream::PushStreamProducer;
    use crate::server::stream::PushStreamWriter;

    struct MockProducer;

    impl PushStreamProducer for MockProducer {
        type Item = i32;
        async fn produce(
            self,
            _writer: PushStreamWriter<impl PushStreamConsumer<Self::Item>>,
        ) -> Result<(), crate::Status> {
            Ok(())
        }
    }

    #[test]
    fn test_streaming_request_creation() {
        let producer = MockProducer;
        let stream = PushStream::new(producer);
        let metadata = Metadata::default();
        let request = StreamingRequest::new(stream, metadata);

        assert_eq!(request.initial_metadata(), &Metadata::default());
    }

    #[test]
    fn test_streaming_request_into_parts() {
        let producer = MockProducer;
        let stream = PushStream::new(producer);
        let metadata = Metadata::default();
        let request = StreamingRequest::new(stream, metadata);

        let (meta, _stream) = request.into_parts();
        assert_eq!(meta, Metadata::default());
    }
}
