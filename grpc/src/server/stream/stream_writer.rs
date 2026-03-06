use crate::Status;

/// A trait for consuming items from a stream.
#[trait_variant::make(Send)]
pub trait PushStreamConsumer<Item: Send> {
    /// Writes an item to the stream.
    async fn write(&mut self, item: Item) -> Result<(), Status>;
}

/// A concrete stream writer that wraps a consumer.
pub struct PushStreamWriter<C> {
    inner: C,
}

impl<C> PushStreamWriter<C> {
    /// Creates a new stream writer.
    pub fn new(inner: C) -> Self {
        Self { inner }
    }

    /// Writes an item to the stream.
    pub async fn write<T: Send>(&mut self, item: T) -> Result<(), Status>
    where
        C: PushStreamConsumer<T>,
    {
        self.inner.write(item).await
    }

    /// Consumes the writer and returns the inner consumer.
    pub fn into_inner(self) -> C {
        self.inner
    }
}

impl<C, T> PushStreamConsumer<T> for PushStreamWriter<C>
where
    C: PushStreamConsumer<T> + Send,
    T: Send,
{
    async fn write(&mut self, item: T) -> Result<(), Status> {
        self.inner.write(item).await
    }
}

#[cfg(test)]

mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockConsumer<T> {
        items: Arc<Mutex<Vec<T>>>,
    }

    impl<T> MockConsumer<T> {
        fn new() -> Self {
            Self {
                items: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl<T: Send> PushStreamConsumer<T> for MockConsumer<T> {
        async fn write(&mut self, item: T) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_push_stream_writer() {
        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let mut writer = PushStreamWriter::new(consumer);

        writer.write(1).await.unwrap();
        writer.write(2).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_push_stream_writer_with_references() {
        // Data must outlive the consumer/writer since we are storing references to it
        let v1 = "value 1".to_string();
        let v2 = "value 2".to_string();

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let mut writer = PushStreamWriter::new(consumer);

        writer.write(v1.as_str()).await.unwrap();
        writer.write(v2.as_str()).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec!["value 1", "value 2"]);
    }

    #[tokio::test]
    async fn test_push_stream_writer_with_mut_references() {
        let mut v1 = "value 1".to_string();
        let mut v2 = "value 2".to_string();

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let mut writer = PushStreamWriter::new(consumer);

        writer.write(&mut v1).await.unwrap();
        writer.write(&mut v2).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(*items[0], "value 1");
        assert_eq!(*items[1], "value 2");
    }

    // A mock equivalent of what prost/protobuf generates for MessageMut wrapper structs.
    // It encapsulates a mutable reference, but is passed by value itself.
    #[derive(Debug, PartialEq, Eq)]
    struct MockMessageMut<'a> {
        _data: &'a mut String,
    }

    #[tokio::test]
    async fn test_push_stream_writer_with_protobuf_mut() {
        let mut s1 = "msg 1".to_string();
        let mut s2 = "msg 2".to_string();

        let m1 = MockMessageMut { _data: &mut s1 };
        let m2 = MockMessageMut { _data: &mut s2 };

        let consumer = MockConsumer::new();
        let items = consumer.items.clone();
        let mut writer = PushStreamWriter::new(consumer);

        // We pass the wrapper struct by value, which transfers the encapsulated
        // mutable reference to the writer and subsequently to the consumer.
        writer.write(m1).await.unwrap();
        writer.write(m2).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(items.len(), 2);
    }
}
