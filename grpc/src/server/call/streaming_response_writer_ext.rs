use crate::server::call::metadata_writer::TrailingMetadataWriter;
use crate::server::call::{Metadata, StreamingResponseWriter};
use crate::server::stream::PushStreamConsumer;
use crate::Status;
use std::future::Future;
use std::marker::PhantomData;

/// Extension trait for `StreamingResponseWriter`.
pub trait StreamingResponseWriterExt<T>: Sized {
    /// Maps items to be written to the stream using the provided async function.
    fn map_message<U, F, Fut>(self, f: F) -> MapMessageStreamingResponseWriter<Self, F, U>
    where
        U: Send,
        F: FnMut(U) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, Status>> + Send;

    /// Maps initial metadata using the provided async function.
    fn map_initial_metadata<F, Fut>(self, f: F) -> MapInitialMetadata<Self, F>
    where
        F: FnMut(Metadata) -> Fut + Send + Sync,
        Fut: Future<Output = Result<Metadata, Status>> + Send;

    /// Maps trailing metadata using the provided async function.
    fn map_trailing_metadata<F, Fut>(self, f: F) -> MapTrailingMetadata<Self, F>
    where
        F: FnMut(Metadata) -> Fut + Send + Sync,
        Fut: Future<Output = Result<Metadata, Status>> + Send;
}

impl<T, W> StreamingResponseWriterExt<T> for W
where
    T: std::marker::Send,
    W: StreamingResponseWriter<T>,
{
    fn map_message<U, F, Fut>(self, f: F) -> MapMessageStreamingResponseWriter<Self, F, U>
    where
        U: Send,
        F: FnMut(U) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, Status>> + Send,
    {
        MapMessageStreamingResponseWriter {
            inner: self,
            f,
            _phantom: PhantomData,
        }
    }

    fn map_initial_metadata<F, Fut>(self, f: F) -> MapInitialMetadata<Self, F>
    where
        F: FnMut(Metadata) -> Fut + Send + Sync,
        Fut: Future<Output = Result<Metadata, Status>> + Send,
    {
        MapInitialMetadata { inner: self, f }
    }

    fn map_trailing_metadata<F, Fut>(self, f: F) -> MapTrailingMetadata<Self, F>
    where
        F: FnMut(Metadata) -> Fut + Send + Sync,
        Fut: Future<Output = Result<Metadata, Status>> + Send,
    {
        MapTrailingMetadata { inner: self, f }
    }
}

/// A wrapper that maps items before writing them to the inner writer.
pub struct MapMessageStreamingResponseWriter<W, F, U> {
    inner: W,
    f: F,
    _phantom: PhantomData<fn(U)>,
}

impl<T, U, W, F, Fut> StreamingResponseWriter<U> for MapMessageStreamingResponseWriter<W, F, U>
where
    T: Send,
    U: Send,
    W: StreamingResponseWriter<T> + Send,
    F: FnMut(U) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, Status>> + Send,
{
    type MessageWriter = MapMessageMessageWriter<W::MessageWriter, F, U>;
    type TrailerWriter = W::TrailerWriter;

    async fn send_initial_metadata(
        self,
        metadata: Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
        let (msg, trl) = self.inner.send_initial_metadata(metadata).await?;
        Ok((
            MapMessageMessageWriter {
                inner: msg,
                f: self.f,
                _phantom: PhantomData,
            },
            trl,
        ))
    }
}

pub struct MapMessageMessageWriter<M, F, U> {
    inner: M,
    f: F,
    _phantom: PhantomData<fn() -> U>,
}

impl<T, U, M, F, Fut> PushStreamConsumer<U> for MapMessageMessageWriter<M, F, U>
where
    T: Send,
    U: Send,
    M: PushStreamConsumer<T> + Send,
    F: FnMut(U) -> Fut + Send + Sync,
    Fut: Future<Output = Result<T, Status>> + Send,
{
    async fn write(&mut self, item: U) -> Result<(), Status> {
        let mapped = (self.f)(item).await?;
        self.inner.write(mapped).await
    }
}

/// A wrapper that maps initial metadata.
pub struct MapInitialMetadata<W, F> {
    inner: W,
    f: F,
}

impl<T, W, F, Fut> StreamingResponseWriter<T> for MapInitialMetadata<W, F>
where
    T: Send,
    W: StreamingResponseWriter<T> + Send,
    F: FnMut(Metadata) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Metadata, Status>> + Send,
{
    type MessageWriter = W::MessageWriter;
    type TrailerWriter = W::TrailerWriter;

    async fn send_initial_metadata(
        mut self,
        metadata: Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
        let mapped = (self.f)(metadata).await?;
        self.inner.send_initial_metadata(mapped).await
    }
}

/// A wrapper that maps trailing metadata.
pub struct MapTrailingMetadata<W, F> {
    inner: W,
    f: F,
}

impl<T, W, F, Fut> StreamingResponseWriter<T> for MapTrailingMetadata<W, F>
where
    T: Send,
    W: StreamingResponseWriter<T> + Send,
    F: FnMut(Metadata) -> Fut + Send + Sync + Clone,
    Fut: Future<Output = Result<Metadata, Status>> + Send,
{
    type MessageWriter = W::MessageWriter;
    type TrailerWriter = MapTrailingMetadataTrailerWriter<W::TrailerWriter, F>;

    async fn send_initial_metadata(
        self,
        metadata: Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
        let (msg, trl) = self.inner.send_initial_metadata(metadata).await?;
        Ok((
            msg,
            MapTrailingMetadataTrailerWriter {
                inner: trl,
                f: self.f,
            },
        ))
    }
}

pub struct MapTrailingMetadataTrailerWriter<Tr, F> {
    inner: Tr,
    f: F,
}

impl<Tr, F, Fut> TrailingMetadataWriter for MapTrailingMetadataTrailerWriter<Tr, F>
where
    Tr: TrailingMetadataWriter + Send,
    F: FnMut(Metadata) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Metadata, Status>> + Send,
{
    async fn send_trailing_metadata(mut self, trailers: Metadata) -> Result<(), Status> {
        let mapped = (self.f)(trailers).await?;
        self.inner.send_trailing_metadata(mapped).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::call::metadata_writer::{InitialMetadataWriter, TrailingMetadataWriter};
    use crate::server::call::test_util::StreamingResponseImpl;
    use crate::server::call::Metadata;
    use crate::server::stream::PushStreamConsumer;
    use crate::server::stream::PushStreamWriter;
    use crate::Status;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockMetadataWriter {
        metadata: Arc<Mutex<Option<Metadata>>>,
    }

    impl MockMetadataWriter {
        fn new() -> Self {
            Self {
                metadata: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl InitialMetadataWriter for MockMetadataWriter {
        async fn send_initial_metadata(self, metadata: Metadata) -> Result<(), Status> {
            *self.metadata.lock().unwrap() = Some(metadata);
            Ok(())
        }
    }

    impl TrailingMetadataWriter for MockMetadataWriter {
        async fn send_trailing_metadata(self, metadata: Metadata) -> Result<(), Status> {
            *self.metadata.lock().unwrap() = Some(metadata);
            Ok(())
        }
    }

    struct MockPushStreamConsumer {
        items: Arc<Mutex<Vec<i32>>>,
    }

    impl MockPushStreamConsumer {
        fn new() -> Self {
            Self {
                items: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl PushStreamConsumer<i32> for MockPushStreamConsumer {
        async fn write(&mut self, item: i32) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_map_message() {
        let consumer = MockPushStreamConsumer::new();
        let items = consumer.items.clone();
        let stream_writer = PushStreamWriter::new(consumer);
        let initial_writer = MockMetadataWriter::new();
        let trailing_writer = MockMetadataWriter::new();
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let writer = writer.map_message(|x| async move { Ok(x * 2) });

        let (mut msg_writer, _trailer_writer) = writer
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();
        msg_writer.write(10).await.unwrap();

        assert_eq!(*items.lock().unwrap(), vec![20]);
    }

    #[tokio::test]
    async fn test_map_initial_metadata() {
        let consumer = MockPushStreamConsumer::new();
        let stream_writer = PushStreamWriter::new(consumer);
        let initial_writer = MockMetadataWriter::new();
        let captured_metadata = initial_writer.metadata.clone();
        let trailing_writer = MockMetadataWriter::new();
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let writer = writer.map_initial_metadata(|mut md| async move {
            md.inner.insert(
                http::header::HeaderName::from_static("test-key"),
                "test-val".parse().unwrap(),
            );
            Ok(md)
        });

        writer
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();

        let md = captured_metadata.lock().unwrap().take().unwrap();
        assert_eq!(md.inner.get("test-key").unwrap(), "test-val");
    }

    #[tokio::test]
    async fn test_map_trailing_metadata() {
        let consumer = MockPushStreamConsumer::new();
        let stream_writer = PushStreamWriter::new(consumer);
        let initial_writer = MockMetadataWriter::new();
        let trailing_writer = MockMetadataWriter::new();
        let captured_trailers = trailing_writer.metadata.clone();
        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let writer = writer.map_trailing_metadata(|mut md| async move {
            md.inner.insert(
                http::header::HeaderName::from_static("trailer-key"),
                "trailer-val".parse().unwrap(),
            );
            Ok(md)
        });

        let (_msg_writer, trailer_writer) = writer
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await
            .unwrap();

        let md = captured_trailers.lock().unwrap().take().unwrap();
        assert_eq!(md.inner.get("trailer-key").unwrap(), "trailer-val");
    }

    #[tokio::test]
    async fn test_composition() {
        let consumer = MockPushStreamConsumer::new();
        let items = consumer.items.clone();
        let stream_writer = PushStreamWriter::new(consumer);

        let initial_writer = MockMetadataWriter::new();
        let captured_initial = initial_writer.metadata.clone();

        let trailing_writer = MockMetadataWriter::new();
        let captured_trailing = trailing_writer.metadata.clone();

        let writer = StreamingResponseImpl::new(stream_writer, initial_writer, trailing_writer);

        let writer = writer
            .map_initial_metadata(|mut md| async move {
                md.inner.insert(
                    http::header::HeaderName::from_static("init"),
                    "1".parse().unwrap(),
                );
                Ok(md)
            })
            .map_message(|x| async move { Ok(x + 1) })
            .map_trailing_metadata(|mut md| async move {
                md.inner.insert(
                    http::header::HeaderName::from_static("trail"),
                    "2".parse().unwrap(),
                );
                Ok(md)
            });

        let (mut msg_writer, trailer_writer) = writer
            .send_initial_metadata(Metadata::default())
            .await
            .unwrap();
        msg_writer.write(10).await.unwrap();
        trailer_writer
            .send_trailing_metadata(Metadata::default())
            .await
            .unwrap();

        assert_eq!(
            captured_initial
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .inner
                .get("init")
                .unwrap(),
            "1"
        );
        assert_eq!(*items.lock().unwrap(), vec![11]);
        assert_eq!(
            captured_trailing
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .inner
                .get("trail")
                .unwrap(),
            "2"
        );
    }
}
