use crate::server::stream::stream_writer::PushStreamWriter;
use crate::server::stream::PushStreamConsumer;
use crate::Status;

/// A trait for producing items into a stream writer.
#[trait_variant::make(Send)]
pub trait PushStreamProducer {
    /// The type of item produced.
    type Item: Send;

    /// Produces items into the writer.
    async fn produce(
        self,
        writer: PushStreamWriter<impl PushStreamConsumer<Self::Item> + Send>,
    ) -> Result<(), Status>;
}

/// A stream that wraps a producer.
pub struct PushStream<P> {
    inner: P,
}

impl<P> PushStream<P>
where
    P: PushStreamProducer,
{
    /// Creates a new stream from a producer.
    pub fn new(producer: P) -> Self {
        Self { inner: producer }
    }

    /// Runs the stream, driving the producer to write to the provided writer.
    pub async fn run(
        self,
        writer: PushStreamWriter<impl PushStreamConsumer<P::Item> + Send>,
    ) -> Result<(), Status> {
        self.inner.produce(writer).await
    }

    /// Consumes the stream and returns the inner producer.
    pub fn into_inner(self) -> P {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::stream::stream_writer::PushStreamConsumer;
    use std::sync::{Arc, Mutex};

    struct MockProducer {
        items: Vec<i32>,
    }

    impl PushStreamProducer for MockProducer {
        type Item = i32;

        async fn produce(
            self,
            mut writer: PushStreamWriter<impl PushStreamConsumer<Self::Item> + Send>,
        ) -> Result<(), Status> {
            for item in self.items {
                writer.write(item).await?;
            }
            Ok(())
        }
    }

    struct MockConsumer {
        items: Arc<Mutex<Vec<i32>>>,
    }

    impl PushStreamConsumer<i32> for MockConsumer {
        async fn write(&mut self, item: i32) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_push_stream() {
        let producer = MockProducer {
            items: vec![1, 2, 3],
        };
        let stream = PushStream::new(producer);

        let consumer = MockConsumer {
            items: Arc::new(Mutex::new(Vec::new())),
        };
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        stream.run(writer).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec![1, 2, 3]);
    }

    // Test case 1: Producing and consuming owned values
    struct OwnedProducer {
        items: Vec<String>,
    }

    impl PushStreamProducer for OwnedProducer {
        type Item = String;

        async fn produce(
            self,
            mut writer: PushStreamWriter<impl PushStreamConsumer<Self::Item> + Send>,
        ) -> Result<(), Status> {
            for item in self.items {
                writer.write(item).await?;
            }
            Ok(())
        }
    }

    struct OwnedConsumer {
        items: Arc<Mutex<Vec<String>>>,
    }

    impl PushStreamConsumer<String> for OwnedConsumer {
        async fn write(&mut self, item: String) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_owned_values() {
        let producer = OwnedProducer {
            items: vec!["hello".to_string(), "world".to_string()],
        };
        let stream = PushStream::new(producer);

        let consumer = OwnedConsumer {
            items: Arc::new(Mutex::new(Vec::new())),
        };
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        stream.run(writer).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec!["hello".to_string(), "world".to_string()]);
    }

    // Test case 2: Producing and consuming references
    struct RefProducer<'a> {
        items: Vec<&'a str>,
    }

    impl<'a> PushStreamProducer for RefProducer<'a> {
        type Item = &'a str;

        async fn produce(
            self,
            mut writer: PushStreamWriter<impl PushStreamConsumer<Self::Item> + Send>,
        ) -> Result<(), Status> {
            for item in self.items {
                writer.write(item).await?;
            }
            Ok(())
        }
    }

    struct RefConsumer<'a> {
        items: Arc<Mutex<Vec<&'a str>>>,
    }

    impl<'a> PushStreamConsumer<&'a str> for RefConsumer<'a> {
        async fn write(&mut self, item: &'a str) -> Result<(), Status> {
            self.items.lock().unwrap().push(item);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_references() {
        let data = ["foo", "bar"];
        let producer = RefProducer {
            items: vec![data[0], data[1]],
        };
        let stream = PushStream::new(producer);

        let consumer = RefConsumer {
            items: Arc::new(Mutex::new(Vec::new())),
        };
        let items = consumer.items.clone();
        let writer = PushStreamWriter::new(consumer);

        stream.run(writer).await.unwrap();

        let items = items.lock().unwrap();
        assert_eq!(*items, vec!["foo", "bar"]);
    }
}
